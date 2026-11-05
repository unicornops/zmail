//! Outbox jobs whose send was interrupted (#403): Zmail stopped mid-send
//! (a crash, a killed command, a force-quit app, a machine that never woke
//! up), and the job was left `Sending`, which nothing else moves it out of.
//!
//! The message may or may not have reached the server, so what the job
//! records must come from the server, never from a guess:
//!
//! - **Proton**: the draft the send creates carries the message's
//!   Message-ID as its ExternalID, which survives sending. A sent message
//!   with it means the job is `Sent`; a draft alone means the send never
//!   happened, so that draft is sent; nothing means the job goes back in
//!   the queue.
//! - **JMAP**: the copy imported into Sent carries `$submitpending` until
//!   the server accepts the submission and swaps it for `$submitted` in
//!   the same call. `$submitted` means `Sent`; `$submitpending` alone means
//!   the copy is submitted now; no copy means the queue. A copy with
//!   neither keyword (imported before Zmail marked copies) proves nothing,
//!   so it is treated as SMTP.
//! - **SMTP**: the server keeps nothing to ask about (a copy in Sent may be
//!   appended after the submission, or never), so the job fails with
//!   [`INTERRUPTED_SEND_MESSAGE`], which asks the user to check Sent before
//!   retrying: at most once.
//!
//! When the server cannot be asked (offline, refused sign-in), the job is
//! left as it is and checked again [`INTERRUPTED_SEND_RECHECK`] later.
//!
//! A send can be in the same doubt without Zmail stopping: the request to
//! send timed out, or the connection dropped, after the server may have
//! acted on it (#405). The adapter marks such a failure
//! [`zmail_core::UnconfirmedSend`], the job is marked for good
//! (`LocalStore::mark_outbox_send_unconfirmed`), and every later attempt
//! makes the same lookup ([`SyncService::find_outgoing`]) before it sends
//! anything: sent means `Sent`, a leftover is finished
//! ([`SyncService::resume_send`]), no trace means a fresh send, and a
//! lookup that fails is retried later like any failed attempt, never
//! followed by a send.
//!
//! Only the process holding the sync lock (ADR 0007) settles interrupted
//! sends, at sync start or takeover before its first outbox pass, then on
//! every pass. A job counts as interrupted once nothing has renewed its
//! claim for [`INTERRUPTED_SEND_AFTER`]: every sender renews it every
//! [`OUTBOX_HEARTBEAT_INTERVAL`](crate::OUTBOX_HEARTBEAT_INTERVAL) while
//! its send runs, including a `--force` command sending beside the
//! holder, so a live send is never taken for an interrupted one however
//! long it takes. The jobs this process is sending are never touched.

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use zmail_core::{
    INTERRUPTED_SEND_MESSAGE, PendingOutboxJob, SubmissionKind, time::parse_timestamp,
};
use zmail_store::InterruptedOutboxJob;

use crate::{SendingJob, SyncService};

/// How long a `Sending` job's sender may stay silent before the job counts
/// as interrupted: six missed renewals. A crashed app's message is settled
/// this long after the crash at the latest.
pub(crate) const INTERRUPTED_SEND_AFTER: Duration = Duration::from_secs(90);
/// How long after a check that could not reach the server the same job is
/// checked again.
const INTERRUPTED_SEND_RECHECK: Duration = Duration::from_secs(60);

/// What became of an interrupted send, as far as the server shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InterruptedSend {
    /// The message went out.
    Sent,
    /// The message never reached the server: send it again.
    NotSent,
    /// The server cannot tell: fail the job and let the user decide.
    Unknown,
    /// The send stopped part way, and what it left can be sent as it is.
    Resume(ResumeSend),
}

/// What an interrupted send left on the server to finish from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ResumeSend {
    /// A JMAP copy imported and never submitted: its id.
    JmapImported(String),
    /// A Proton draft never sent: its ID.
    #[cfg_attr(not(feature = "proton"), allow(dead_code))]
    ProtonDraft(String),
}

impl SyncService {
    /// Sets how long a `Sending` job's sender may stay silent before the
    /// job counts as an interrupted send ([`INTERRUPTED_SEND_AFTER`] by
    /// default). Tests use zero to stand in for a send interrupted long
    /// ago.
    pub fn set_interrupted_send_after(&self, after: Duration) {
        *self
            .inner
            .interrupted_send_after
            .lock()
            .expect("interrupted send mutex should not be poisoned") = after;
    }

    /// Settles the outbox jobs (of `account_id`, or every account's) whose
    /// send was interrupted, if this process holds the sync lock, and
    /// returns how many it settled: found sent, sent now or failed. Ones
    /// found never sent go back in the queue for the next pass, and ones
    /// the server could not be asked about are left for a later check.
    pub(crate) async fn recover_interrupted_sends(&self, account_id: Option<i64>) -> Result<usize> {
        if !self.holds_sync_lock() {
            return Ok(0);
        }
        let mut settled = 0;
        while let Some((interrupted, _sending)) = self.claim_interrupted_send(account_id)? {
            let job = &interrupted.job;
            let found = self
                .renewing_claim(
                    job.job_id,
                    self.find_outgoing(job, interrupted.claimed_at.as_deref()),
                )
                .await;
            let store = &self.inner.store;
            match found {
                Ok(InterruptedSend::Sent) => {
                    store.record_outbox_sent(job.job_id)?;
                    settled += 1;
                }
                Ok(InterruptedSend::NotSent) => store.requeue_interrupted_outbox_job(job.job_id)?,
                Ok(InterruptedSend::Unknown) => {
                    store.record_outbox_failure(job.job_id, INTERRUPTED_SEND_MESSAGE)?;
                    settled += 1;
                }
                Ok(InterruptedSend::Resume(resume)) => {
                    let sent = self
                        .renewing_claim(job.job_id, self.resume_send(job, resume))
                        .await;
                    self.record_attempt(job, sent)?;
                    settled += 1;
                }
                Err(_) => {
                    self.inner
                        .interrupted_rechecks
                        .lock()
                        .expect("interrupted send mutex should not be poisoned")
                        .insert(job.job_id, Instant::now() + INTERRUPTED_SEND_RECHECK);
                }
            }
        }
        Ok(settled)
    }

    /// Claims the next interrupted send to check, and marks it as being
    /// handled by this process until the returned guard is dropped. Jobs
    /// this process is sending, and ones whose check failed lately, are
    /// skipped.
    fn claim_interrupted_send(
        &self,
        account_id: Option<i64>,
    ) -> Result<Option<(InterruptedOutboxJob, SendingJob<'_>)>> {
        let mut sending = self
            .inner
            .outbox_sending
            .lock()
            .expect("outbox sending mutex should not be poisoned");
        let mut skip = sending.keys().copied().collect::<Vec<_>>();
        {
            let now = Instant::now();
            let mut rechecks = self
                .inner
                .interrupted_rechecks
                .lock()
                .expect("interrupted send mutex should not be poisoned");
            rechecks.retain(|_, at| *at > now);
            skip.extend(rechecks.keys());
        }
        let after = *self
            .inner
            .interrupted_send_after
            .lock()
            .expect("interrupted send mutex should not be poisoned");
        let Some(interrupted) = self
            .inner
            .store
            .claim_interrupted_outbox_job(account_id, after, &skip)?
        else {
            return Ok(None);
        };
        let job = &interrupted.job;
        sending.insert(job.job_id, job.target.account_id);
        let guard = SendingJob {
            inner: &self.inner,
            job_id: job.job_id,
        };
        Ok(Some((interrupted, guard)))
    }

    /// Asks the server what became of a send of `job` that may or may not
    /// have happened: an interrupted one, claimed at `claimed_at`, or one
    /// that failed without proving the server refused it
    /// ([`zmail_core::UnconfirmedSend`]). An error means it could not be
    /// asked.
    pub(crate) async fn find_outgoing(
        &self,
        job: &PendingOutboxJob,
        claimed_at: Option<&str>,
    ) -> Result<InterruptedSend> {
        // Every claim gives the message its Message-ID; without one there
        // is nothing to look for.
        let Some(message_id) = job.message.message_id.as_deref() else {
            return Ok(InterruptedSend::Unknown);
        };
        match job.target.outgoing.kind {
            SubmissionKind::Smtp => Ok(InterruptedSend::Unknown),
            SubmissionKind::Jmap => {
                let target = self.outbox_account(job)?;
                let secret = self.outbox_secret(job, &target).await?;
                let claimed_at = claimed_at
                    .and_then(parse_timestamp)
                    .map(|claimed_at| claimed_at.timestamp());
                let trace = zmail_jmap::find_submitted_message(
                    &job.target.endpoint,
                    &job.target.username,
                    &secret,
                    target.auth,
                    message_id,
                    claimed_at,
                )
                .await?;
                Ok(match trace {
                    zmail_jmap::SubmissionTrace::Submitted => InterruptedSend::Sent,
                    zmail_jmap::SubmissionTrace::Imported(email_id) => {
                        InterruptedSend::Resume(ResumeSend::JmapImported(email_id))
                    }
                    zmail_jmap::SubmissionTrace::Unknown => InterruptedSend::Unknown,
                    zmail_jmap::SubmissionTrace::None => InterruptedSend::NotSent,
                })
            }
            SubmissionKind::Proton => {
                let target = self.outbox_account(job)?;
                self.find_interrupted_proton_send(&target, message_id).await
            }
        }
    }

    /// Finishes a send from what it left on the server: a delivery
    /// attempt, recorded as one.
    pub(crate) async fn resume_send(
        &self,
        job: &PendingOutboxJob,
        resume: ResumeSend,
    ) -> Result<()> {
        let target = self.outbox_account(job)?;
        match resume {
            ResumeSend::JmapImported(email_id) => {
                let secret = self.outbox_secret(job, &target).await?;
                zmail_jmap::submit_imported_message(
                    &job.target.endpoint,
                    &job.target.username,
                    &secret,
                    target.auth,
                    &job.target.identity_email,
                    &email_id,
                    &job.message,
                )
                .await
                .context("failed to submit the copy an interrupted send imported")
            }
            ResumeSend::ProtonDraft(draft_id) => {
                self.resume_proton_send(&target, &draft_id, &job.message)
                    .await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::{BufRead, BufReader, Write},
        net::TcpListener,
        sync::{Arc, Mutex},
        thread,
        time::{Duration, Instant},
    };

    use zmail_core::{
        AuthKind, CachePolicySummary, INTERRUPTED_SEND_MESSAGE, NewAccount, NewDraft, NewIdentity,
        OutboxJobState, OutboxJobSummary, OutgoingConfig, ProtocolKind, SubmissionKind,
        SubmissionSecurity,
    };
    use zmail_security::{InMemorySecretStore, SecretStore};
    use zmail_store::LocalStore;

    use crate::SyncService;

    /// An SMTP server that takes every message it is sent, waiting `delay`
    /// before it answers each message's end. It keeps the subjects of the
    /// messages it took, and counts connections.
    struct SmtpServer {
        port: u16,
        subjects: Arc<Mutex<Vec<String>>>,
        connections: Arc<Mutex<usize>>,
    }

    impl SmtpServer {
        fn start(delay: Duration) -> Self {
            let listener = TcpListener::bind(("127.0.0.1", 0)).expect("the mock binds");
            let port = listener.local_addr().expect("the mock has a port").port();
            let subjects = Arc::new(Mutex::new(Vec::new()));
            let connections = Arc::new(Mutex::new(0));
            let (taken, count) = (subjects.clone(), connections.clone());
            thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else { break };
                    *count.lock().expect("the count") += 1;
                    let mut writer = stream.try_clone().expect("the stream clones");
                    let mut reader = BufReader::new(stream);
                    let _ = writer.write_all(b"220 mock.example.test ESMTP\r\n");
                    let (mut in_data, mut subject) = (false, String::new());
                    let mut line = String::new();
                    while reader.read_line(&mut line).is_ok_and(|read| read > 0) {
                        let text = line.trim_end().to_owned();
                        line.clear();
                        let upper = text.to_ascii_uppercase();
                        let reply: &[u8] = if in_data {
                            if let Some(value) = text.strip_prefix("Subject: ") {
                                subject = value.to_owned();
                            }
                            if text != "." {
                                continue;
                            }
                            in_data = false;
                            thread::sleep(delay);
                            taken.lock().expect("the subjects").push(subject.clone());
                            b"250 2.0.0 queued\r\n"
                        } else if upper.starts_with("EHLO") {
                            b"250-mock.example.test\r\n250 AUTH PLAIN\r\n"
                        } else if upper.starts_with("AUTH") {
                            b"235 2.7.0 accepted\r\n"
                        } else if upper.starts_with("DATA") {
                            in_data = true;
                            b"354 go ahead\r\n"
                        } else if upper.starts_with("QUIT") {
                            b"221 bye\r\n"
                        } else {
                            b"250 ok\r\n"
                        };
                        if writer.write_all(reply).is_err() || upper.starts_with("QUIT") {
                            break;
                        }
                    }
                }
            });
            Self {
                port,
                subjects,
                connections,
            }
        }

        fn subjects(&self) -> Vec<String> {
            self.subjects.lock().expect("the subjects").clone()
        }
    }

    /// A store with an SMTP account sending through `port`, its password,
    /// and one queued message: (store, secrets, account, job).
    fn queued_message(port: u16) -> (Arc<LocalStore>, Arc<dyn SecretStore>, i64, i64) {
        let store = Arc::new(LocalStore::open_ephemeral().expect("store should open"));
        let account_id = store
            .insert_account(&NewAccount {
                display_name: "Test Account".to_owned(),
                protocol: ProtocolKind::Imap,
                auth: AuthKind::Password,
                endpoint: "imaps://mail.example.test:993".to_owned(),
                username: "user@example.test".to_owned(),
                cache_policy: CachePolicySummary::performance_first(),
                outgoing: Some(OutgoingConfig {
                    kind: SubmissionKind::Smtp,
                    host: Some("127.0.0.1".to_owned()),
                    port: Some(port),
                    username: Some("user@example.test".to_owned()),
                    security: Some(SubmissionSecurity::Plain),
                    hello_name: "localhost".to_owned(),
                }),
                identities: vec![NewIdentity {
                    display_name: "Test Account".to_owned(),
                    email_address: "user@example.test".to_owned(),
                    signature: String::new(),
                    is_primary: true,
                }],
            })
            .expect("the account should be inserted");
        let secrets: Arc<dyn SecretStore> = InMemorySecretStore::shared();
        secrets
            .set_account_secret(account_id, "password")
            .expect("the password stores");
        let identity_id = store.list_identities(account_id).expect("identities")[0].id;
        let draft_id = store
            .create_draft(&NewDraft {
                account_id,
                identity_id,
                to: vec!["ada@example.com".to_owned()],
                cc: Vec::new(),
                bcc: Vec::new(),
                subject: "Lunch".to_owned(),
                body: "Noon?".to_owned(),
                threading: Default::default(),
            })
            .expect("a draft");
        let job_id = store.queue_draft(draft_id).expect("queued");
        (store, secrets, account_id, job_id)
    }

    /// A process that claimed the job and stopped before recording how
    /// the send went.
    fn interrupt(store: &LocalStore, job_id: i64) {
        let claimed = store
            .claim_due_outbox_job(None)
            .expect("the claim runs")
            .expect("the job is due");
        assert_eq!(claimed.job_id, job_id);
    }

    fn job(store: &LocalStore) -> OutboxJobSummary {
        store.list_outbox_jobs(None).expect("the outbox")[0].clone()
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("the runtime starts")
    }

    #[test]
    fn an_interrupted_smtp_send_fails_asking_to_check_sent_and_retries_once() {
        let server = SmtpServer::start(Duration::ZERO);
        let (store, secrets, account_id, job_id) = queued_message(server.port);
        interrupt(&store, job_id);

        let runtime = runtime();
        let service =
            SyncService::for_command(runtime.handle().clone(), store.clone(), secrets.clone())
                .expect("the service starts");
        assert!(service.runs_sync(), "it holds the sync lock");
        service.set_interrupted_send_after(Duration::ZERO);
        // The outbox poll may settle it first; the pass waits for that
        // either way.
        service
            .process_outbox_and_wait(Some(account_id))
            .expect("the pass runs");
        let failed = job(&store);
        assert_eq!(failed.state, OutboxJobState::Failed);
        assert_eq!(
            failed.last_error_message.as_deref(),
            Some(INTERRUPTED_SEND_MESSAGE)
        );
        assert_eq!(failed.attempt_count, 1);
        assert_eq!(
            *server.connections.lock().expect("the count"),
            0,
            "an SMTP server keeps nothing to ask about, and nothing is sent again unasked"
        );

        // Retry, as each frontend offers for a failed message, sends it
        // once.
        store.retry_outbox_job(job_id).expect("requeued");
        service
            .process_outbox_and_wait(Some(account_id))
            .expect("the pass runs");
        assert_eq!(job(&store).state, OutboxJobState::Sent);
        assert_eq!(server.subjects(), ["Lunch"]);
    }

    #[test]
    fn only_the_sync_lock_holder_settles_interrupted_sends() {
        let server = SmtpServer::start(Duration::ZERO);
        let (store, secrets, account_id, job_id) = queued_message(server.port);
        interrupt(&store, job_id);
        let runtime = runtime();

        // `--force` beside a Zmail that syncs: it sends, but never decides
        // about another process's job.
        let held = store
            .try_sync_lock()
            .expect("the lock opens")
            .expect("free");
        let forced = SyncService::new(runtime.handle().clone(), store.clone(), secrets.clone());
        forced.set_interrupted_send_after(Duration::ZERO);
        assert_eq!(
            forced
                .process_outbox_and_wait(Some(account_id))
                .expect("the pass runs"),
            0
        );
        assert_eq!(job(&store).state, OutboxJobState::Sending);
        drop(forced);
        drop(held);

        // The holder settles it.
        let holder = SyncService::for_command(runtime.handle().clone(), store.clone(), secrets)
            .expect("the service starts");
        holder.set_interrupted_send_after(Duration::ZERO);
        holder
            .process_outbox_and_wait(Some(account_id))
            .expect("the pass runs");
        assert_eq!(job(&store).state, OutboxJobState::Failed);
    }

    #[test]
    fn a_send_another_process_is_still_making_is_left_alone() {
        let server = SmtpServer::start(Duration::ZERO);
        let (store, secrets, account_id, job_id) = queued_message(server.port);
        // Claimed this moment, as a `--force` command beside it would.
        interrupt(&store, job_id);
        let runtime = runtime();
        let holder = SyncService::for_command(runtime.handle().clone(), store.clone(), secrets)
            .expect("the service starts");
        assert_eq!(
            holder
                .process_outbox_and_wait(Some(account_id))
                .expect("the pass runs"),
            0
        );
        let sending = job(&store);
        assert_eq!(sending.state, OutboxJobState::Sending);
        assert_eq!(sending.attempt_count, 1);
        assert_eq!(*server.connections.lock().expect("the count"), 0);
    }

    #[test]
    fn a_send_this_process_is_making_is_never_touched() {
        let server = SmtpServer::start(Duration::ZERO);
        let (store, secrets, account_id, job_id) = queued_message(server.port);
        interrupt(&store, job_id);
        let runtime = runtime();
        let holder = SyncService::for_command(runtime.handle().clone(), store.clone(), secrets)
            .expect("the service starts");
        holder.set_interrupted_send_after(Duration::ZERO);
        // One of this process's passes has it, however silent it looks.
        holder
            .inner
            .outbox_sending
            .lock()
            .expect("the sending set")
            .insert(job_id, account_id);
        let settled = runtime
            .block_on(holder.recover_interrupted_sends(None))
            .expect("the check runs");
        assert_eq!(settled, 0);
        assert_eq!(job(&store).state, OutboxJobState::Sending);
    }

    #[test]
    fn a_long_send_renews_its_claim_so_it_is_never_taken_for_interrupted() {
        // The server takes three seconds over the message.
        let server = SmtpServer::start(Duration::from_secs(3));
        let (store, secrets, account_id, _) = queued_message(server.port);
        let runtime = runtime();
        let sender = SyncService::new(runtime.handle().clone(), store.clone(), secrets);
        sender.process_outbox(Some(account_id)).expect("scheduled");
        let started = Instant::now();
        while job(&store).state != OutboxJobState::Sending {
            assert!(started.elapsed() < Duration::from_secs(5), "never claimed");
            thread::sleep(Duration::from_millis(10));
        }
        // Long enough that the claim alone would look silent for two
        // seconds; the renewals say otherwise.
        thread::sleep(Duration::from_millis(2_500));
        assert_eq!(job(&store).state, OutboxJobState::Sending);
        assert!(
            store
                .claim_interrupted_outbox_job(None, Duration::from_secs(2), &[])
                .expect("the claim runs")
                .is_none(),
            "a send still running was taken for an interrupted one"
        );
        while job(&store).state != OutboxJobState::Sent {
            assert!(started.elapsed() < Duration::from_secs(10), "never sent");
            thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(server.subjects(), ["Lunch"]);
    }

    #[test]
    fn a_check_that_cannot_reach_the_server_leaves_the_job_for_later() {
        // A JMAP account whose server does not answer: nothing can be
        // learnt, so nothing is decided.
        let store = Arc::new(LocalStore::open_ephemeral().expect("store should open"));
        let closed = TcpListener::bind(("127.0.0.1", 0)).expect("a port");
        let endpoint = format!("http://{}/", closed.local_addr().expect("an address"));
        drop(closed);
        let account_id = store
            .insert_account(&NewAccount {
                display_name: "JMAP".to_owned(),
                protocol: ProtocolKind::Jmap,
                auth: AuthKind::Password,
                endpoint,
                username: "user@example.test".to_owned(),
                cache_policy: CachePolicySummary::performance_first(),
                outgoing: None,
                identities: vec![NewIdentity {
                    display_name: "User".to_owned(),
                    email_address: "user@example.test".to_owned(),
                    signature: String::new(),
                    is_primary: true,
                }],
            })
            .expect("the account");
        let secrets: Arc<dyn SecretStore> = InMemorySecretStore::shared();
        secrets
            .set_account_secret(account_id, "password")
            .expect("the password stores");
        let identity_id = store.list_identities(account_id).expect("identities")[0].id;
        let draft_id = store
            .create_draft(&NewDraft {
                account_id,
                identity_id,
                to: vec!["ada@example.com".to_owned()],
                cc: Vec::new(),
                bcc: Vec::new(),
                subject: "Lunch".to_owned(),
                body: "Noon?".to_owned(),
                threading: Default::default(),
            })
            .expect("a draft");
        let job_id = store.queue_draft(draft_id).expect("queued");
        interrupt(&store, job_id);

        let runtime = runtime();
        let holder = SyncService::for_command(runtime.handle().clone(), store.clone(), secrets)
            .expect("the service starts");
        holder.set_interrupted_send_after(Duration::ZERO);
        let settled = runtime
            .block_on(holder.recover_interrupted_sends(None))
            .expect("the check runs");
        assert_eq!(settled, 0);
        let left = job(&store);
        assert_eq!(left.state, OutboxJobState::Sending);
        assert_eq!(left.last_error_message, None);
        assert!(
            holder
                .inner
                .interrupted_rechecks
                .lock()
                .expect("the rechecks")
                .contains_key(&job_id),
            "checked again later, not on every pass"
        );
    }
}

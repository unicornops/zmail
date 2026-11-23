//! Threads (RFC 8621 section 3): the emails of one conversation.

use crate::{
    capability,
    changes::ChangesObject,
    method::{Get, GetObject, Object},
    record::record,
    request::{Call, IdsReference},
};

use super::Email;

/// The Thread data type (RFC 8621 section 3), for
/// [`Get`]`<Thread>` and [`Changes`](crate::Changes)`<Thread>`.
#[derive(Debug, Clone, Copy)]
pub enum Thread {}

impl Object for Thread {
    const NAME: &'static str = "Thread";
    const CAPABILITY: &'static str = capability::MAIL;
}

impl GetObject for Thread {
    type Record = ThreadRecord;
}

impl ChangesObject for Thread {}

record! {
    /// A thread as the server returns it (RFC 8621 section 3).
    pub struct ThreadRecord {
        /// The thread's id.
        "id" => id: String,
        /// The ids of its emails, oldest first by `receivedAt`.
        "emailIds" => email_ids: Vec<String>,
    }
}

impl Call<Get<Thread>> {
    /// A reference to the ids of every email in the threads this call
    /// returns (`/list/*/emailIds`), for the `ids_from` of an
    /// `Email/get` later in the request.
    pub fn email_ids(&self) -> IdsReference<Email> {
        self.ids_in("emailIds")
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{Request, Response};

    /// The example of RFC 8621 section 3.1.1.
    #[test]
    fn the_thread_get_of_rfc_8621_section_3_1_1_is_written_and_read() {
        let mut request = Request::new();
        let call = request
            .call(Get::<Thread>::new("acme").ids(["f123u4", "f41u44"]))
            .expect("encodes");
        assert_eq!(
            serde_json::to_value(&request.method_calls()[0]).expect("encodes"),
            json!(["Thread/get", {"accountId": "acme", "ids": ["f123u4", "f41u44"]}, "c0"])
        );
        let response: Response = serde_json::from_value(json!({
            "methodResponses": [["Thread/get", {
                "accountId": "acme",
                "state": "f6a7e214",
                "list": [
                    {"id": "f123u4", "emailIds": ["eaa623", "f782cbb"]},
                    {"id": "f41u44", "emailIds": ["82cf7bb"]}
                ],
                "notFound": []
            }, "c0"]],
            "sessionState": "s"
        }))
        .expect("decodes");
        let threads = response.get(&call).expect("answered").list;
        assert_eq!(threads[0].id.as_deref(), Some("f123u4"));
        assert_eq!(
            threads[0].email_ids,
            Some(vec!["eaa623".to_owned(), "f782cbb".to_owned()])
        );
        assert_eq!(threads[1].email_ids, Some(vec!["82cf7bb".to_owned()]));
    }
}

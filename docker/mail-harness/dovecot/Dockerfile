FROM debian:12-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends dovecot-imapd ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Static single-user setup: the account exists only inside the harness.
RUN useradd -m -u 5000 mailuser \
    && mkdir -p /var/mail/mailuser \
    && chown -R 5000:5000 /var/mail

COPY dovecot.conf /etc/dovecot/conf.d/99-zmail.conf
COPY users /etc/dovecot/users
RUN chmod 600 /etc/dovecot/users

EXPOSE 143
CMD ["dovecot", "-F"]

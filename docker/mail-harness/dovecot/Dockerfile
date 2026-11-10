FROM debian:12-slim@sha256:734a72b3d6142a71993b4bc580ca01d8ca687f77a2c422d96a961d3376b179bc

RUN apt-get update \
    && apt-get install -y --no-install-recommends dovecot-imapd ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Static single-user setup: the account exists only inside the harness.
RUN useradd -m -u 5000 mailuser \
    && mkdir -p /var/mail/mailuser \
    && chown -R 5000:5000 /var/mail

COPY dovecot.conf /etc/dovecot/conf.d/99-zmail.conf
COPY auth.conf /etc/dovecot/conf.d/10-auth.conf
COPY users /etc/dovecot/users
RUN chmod 644 /etc/dovecot/users

EXPOSE 143
CMD ["dovecot", "-F"]

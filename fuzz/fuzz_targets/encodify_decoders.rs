#![no_main]
//! Fuzzes the decoders mail-parser 1.0 hands untrusted mail to: encodify's
//! base64, quoted-printable, RFC 2047 encoded words, UTF-7 and percent
//! decoding (SIMD kernels with `unsafe`), and mail-parser's charset and
//! transfer decoding on top of them.
//!
//! Byte 0 is a control byte (the length of bytes already in the output
//! buffer, a stop byte, a slice length); the rest is the payload, fed to
//! every decoder. Besides the sanitizer, it checks that:
//! * every `String` and `&str` is valid UTF-8 (several are built unchecked);
//! * `decoded_len` is exact (mail-parser sizes attachments with it);
//! * appending keeps what the buffer held and an error appends nothing;
//! * a decoder never writes past an exactly sized slice;
//! * mail-parser's tolerant base64 path (`MessagePart::decoded`) stays in
//!   bounds.

use encodify::{base64, hex, qp, rfc2047, utf7};
use libfuzzer_sys::fuzz_target;
use mail_parser::decoders::charsets;
use std::hint::black_box;

fn check(text: &str) {
    assert!(
        std::str::from_utf8(text.as_bytes()).is_ok(),
        "invalid UTF-8: {:?}",
        text.as_bytes()
    );
    black_box(text.chars().count());
}

/// A buffer of exactly `len` bytes, so a sanitizer sees any write past it.
fn exact(len: usize) -> Box<[u8]> {
    vec![0xa5; len].into_boxed_slice()
}

struct Input<'x> {
    data: &'x [u8],
    prefix: Vec<u8>,
    stop: u8,
    slice_len: usize,
}

fn base64_engine(engine: &base64::Base64, name: &str, input: &Input<'_>) {
    let Input {
        data,
        prefix,
        stop,
        slice_len,
    } = input;
    let decoded = engine.decode(data);
    match (&decoded, engine.decoded_len(data)) {
        (Ok(bytes), Ok(len)) => assert_eq!(bytes.len(), len, "{name}: decoded_len"),
        (Err(_), Err(_)) => {}
        (decoded, len) => panic!("{name}: decode {decoded:?} and decoded_len {len:?} disagree"),
    }

    let mut out = prefix.clone();
    out.shrink_to_fit();
    let appended = engine.decode_append(data, &mut out);
    assert_eq!(
        &out[..prefix.len()],
        &prefix[..],
        "{name}: decode_append overwrote the buffer"
    );
    match (&appended, &decoded) {
        (Ok(len), Ok(bytes)) => {
            assert_eq!(*len, bytes.len(), "{name}: decode_append length");
            assert_eq!(
                &out[prefix.len()..],
                &bytes[..],
                "{name}: decode_append bytes"
            );
        }
        (Err(_), Err(_)) => assert_eq!(out.len(), prefix.len(), "{name}: an error appended"),
        _ => panic!("{name}: decode_append and decode disagree"),
    }

    let decoded_len = decoded.as_ref().map_or(0, Vec::len);
    for len in [
        *slice_len,
        decoded_len,
        engine.decoded_len_estimate(data.len()),
    ] {
        let mut buffer = exact(len);
        if let (Ok(written), Ok(bytes)) = (engine.decode_slice(data, &mut buffer), &decoded) {
            assert_eq!(&buffer[..written], &bytes[..], "{name}: decode_slice");
        }
    }
    let mut in_place = data.to_vec().into_boxed_slice();
    if let (Ok(bytes), Ok(expected)) = (engine.decode_in_place(&mut in_place), &decoded) {
        assert_eq!(&bytes[..], &expected[..], "{name}: decode_in_place");
    }
    if let Ok(text) = engine.decode_to_string(data) {
        check(&text);
    }
    if let Ok((_, consumed)) = engine.decode_until(data, *stop) {
        assert!(
            consumed <= data.len(),
            "{name}: decode_until consumed too much"
        );
    }
    let mut out = prefix.clone();
    assert!(engine.decode_prefix(data, &mut out) <= data.len());
    assert_eq!(&out[..prefix.len()], &prefix[..]);
    let mut out = prefix.clone();
    if let Ok(value) = engine.decode_folded(data, &mut out) {
        assert!(value.next <= data.len(), "{name}: decode_folded next");
    }
    assert_eq!(&out[..prefix.len()], &prefix[..]);
}

fn qp_engine(engine: &qp::QuotedPrintable, name: &str, input: &Input<'_>) {
    let Input {
        data,
        prefix,
        slice_len,
        ..
    } = input;
    let decoded = engine.decode(data);
    match (&decoded, engine.decoded_len(data)) {
        (Ok(bytes), Ok(len)) => assert_eq!(bytes.len(), len, "{name}: decoded_len"),
        (Err(_), Err(_)) => {}
        (decoded, len) => panic!("{name}: decode {decoded:?} and decoded_len {len:?} disagree"),
    }
    let mut out = prefix.clone();
    out.shrink_to_fit();
    let appended = engine.decode_append(data, &mut out);
    assert_eq!(
        &out[..prefix.len()],
        &prefix[..],
        "{name}: decode_append overwrote the buffer"
    );
    if let (Ok(len), Ok(bytes)) = (&appended, &decoded) {
        assert_eq!(*len, bytes.len(), "{name}: decode_append length");
        assert_eq!(
            &out[prefix.len()..],
            &bytes[..],
            "{name}: decode_append bytes"
        );
    }
    let decoded_len = decoded.as_ref().map_or(0, |bytes| bytes.len());
    for len in [*slice_len, decoded_len, data.len()] {
        let mut buffer = exact(len);
        if let (Ok(written), Ok(bytes)) = (engine.decode_slice(data, &mut buffer), &decoded) {
            assert_eq!(&buffer[..written], &bytes[..], "{name}: decode_slice");
        }
    }
    if let Ok((_, consumed)) = engine.decode_word(data) {
        assert!(
            consumed <= data.len(),
            "{name}: decode_word consumed too much"
        );
    }
}

/// mail-parser's tolerant base64 decoding (decoders/mod.rs), used when a
/// part's base64 is malformed.
fn base64_tolerant(mut rest: &[u8], out: &mut Vec<u8>) {
    let mut decoder = base64::MIME.decoder();
    loop {
        rest = rest
            .get(decoder.decode_symbols(rest, out)..)
            .unwrap_or_default();
        match rest.split_first() {
            None => break,
            Some((b'=', tail)) => {
                decoder.pad(out);
                rest = tail;
            }
            Some((_, tail)) => rest = tail,
        }
    }
    decoder.finish(out);
}

const CHARSETS: &[&[u8]] = &[
    b"utf-8",
    b"us-ascii",
    b"iso-8859-1",
    b"iso-8859-2",
    b"windows-1252",
    b"windows-1251",
    b"koi8-r",
    b"utf-7",
    b"utf-16",
    b"utf-16le",
    b"utf-16be",
    b"shift_jis",
    b"euc-jp",
    b"iso-2022-jp",
    b"gb2312",
    b"gb18030",
    b"big5",
    b"euc-kr",
    b"x-unknown",
];

fuzz_target!(|bytes: &[u8]| {
    let Some((&control, data)) = bytes.split_first() else {
        return;
    };
    let input = Input {
        data,
        prefix: (0..control & 0x0f).collect(),
        stop: b"\r\n=-?\"<>"[usize::from(control >> 4) % 8],
        slice_len: usize::from(control) * 37 % (data.len() + 8),
    };

    for (engine, name) in [
        (base64::MIME, "MIME"),
        (base64::LENIENT, "LENIENT"),
        (base64::STANDARD, "STANDARD"),
        (base64::STANDARD_NO_PAD, "STANDARD_NO_PAD"),
        (base64::URL_SAFE.any_alphabet().lenient(), "ANY_LENIENT"),
    ] {
        base64_engine(&engine, name, &input);
    }
    let mut out = input.prefix.clone();
    base64_tolerant(data, &mut out);
    assert_eq!(&out[..input.prefix.len()], &input.prefix[..]);

    for (engine, name) in [
        (qp::BODY, "BODY"),
        (qp::BODY.strict(), "BODY.strict"),
        (qp::Q_TEXT, "Q_TEXT"),
        (qp::Q_PHRASE, "Q_PHRASE"),
    ] {
        qp_engine(&engine, name, &input);
    }

    // RFC 2047 encoded words, one at a time and as header text.
    let mut at = 0;
    while at < data.len() {
        match rfc2047::EncodedWord::parse(&data[at..]) {
            Some((word, consumed)) => {
                assert!(
                    consumed > 0 && at + consumed <= data.len(),
                    "encoded word length"
                );
                let mut out = input.prefix.clone();
                let _ = word.decode_append(&mut out);
                assert_eq!(&out[..input.prefix.len()], &input.prefix[..]);
                at += consumed;
            }
            None => at += 1,
        }
    }
    check(&rfc2047::decode_text(data, rfc2047::utf8_charset));
    check(&rfc2047::decode_text(data, charsets::decode_append));
    let mut out = String::from("ü");
    let appended = rfc2047::decode_text_append(data, charsets::decode_append, &mut out);
    check(&out);
    assert_eq!(out.len(), "ü".len() + appended);

    for (engine, name) in [
        (utf7::IMAP, "IMAP"),
        (utf7::MAIL, "MAIL"),
        (utf7::MAIL.lenient(), "MAIL.lenient"),
    ] {
        if let Ok(text) = engine.decode(data) {
            check(&text);
        }
        let mut out = String::from("ß");
        match engine.decode_append(data, &mut out) {
            Ok(appended) => assert_eq!(out.len(), "ß".len() + appended, "{name}"),
            Err(_) => assert_eq!(out, "ß", "{name}: an error appended"),
        }
        check(&engine.decode_lossy(data));
    }

    // RFC 2231 parameter values.
    let mut out = input.prefix.clone();
    let _ = hex::PERCENT.decode_append(data, &mut out);
    assert_eq!(&out[..input.prefix.len()], &input.prefix[..]);

    for encoding in [
        mail_parser::Encoding::Base64,
        mail_parser::Encoding::QuotedPrintable,
    ] {
        let decoded = encoding.decode(data);
        assert_eq!(
            decoded.len(),
            encoding.decoded_len(data),
            "{encoding:?}: decoded_len"
        );
        black_box(encoding.decode_checked(data));
    }
    let label = CHARSETS[usize::from(control) % CHARSETS.len()];
    check(&charsets::decode(label, data));
    let mut out = String::from("é");
    charsets::decode_append(label, data, &mut out);
    check(&out);
    assert!(out.starts_with('é'));
    if let Some(charset) = mail_parser::Charset::from_label(label) {
        check(&charset.decode(data));
        check(&charset.decode_checked(data).0);
    }
});

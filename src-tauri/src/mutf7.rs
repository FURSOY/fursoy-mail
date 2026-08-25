//! Modified UTF-7 (RFC 3501 §5.1.3), the encoding IMAP carries mailbox names
//! and Gmail's `X-GM-LABELS` values in. Neither the `imap` crate nor
//! `imap-proto` touches it, so without this a folder named "Önemli" arrives as
//! `&AMY-nemli`, gets shown in the sidebar exactly like that, and any name this
//! app sends back is rejected or creates a second, differently-named folder.
//!
//! Everything above this boundary stores and shows real Unicode; only the wire
//! side is encoded.

use base64::engine::{general_purpose::NO_PAD, GeneralPurpose};
use base64::{alphabet::Alphabet, Engine as _};
use std::sync::OnceLock;

/// The same BASE64 alphabet as RFC 4648 with `,` in place of `/`, because `/`
/// is a mailbox hierarchy delimiter on most servers.
fn modified_base64() -> &'static GeneralPurpose {
    static ENGINE: OnceLock<GeneralPurpose> = OnceLock::new();
    ENGINE.get_or_init(|| {
        let alphabet =
            Alphabet::new("ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+,")
                .expect("the modified UTF-7 alphabet is valid");
        GeneralPurpose::new(&alphabet, NO_PAD)
    })
}

/// Turns a wire name into text. Anything that is not valid modified UTF-7 is
/// returned as it came: a name this app cannot decode is still a name the user
/// has to be able to read and match against what the server shows them.
pub fn decode(value: &str) -> String {
    if !value.contains('&') {
        return value.to_string();
    }
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '&' {
            out.push(ch);
            continue;
        }
        let mut run = String::new();
        let mut terminated = false;
        for next in chars.by_ref() {
            if next == '-' {
                terminated = true;
                break;
            }
            run.push(next);
        }
        // `&-` is how a literal ampersand is written.
        if run.is_empty() && terminated {
            out.push('&');
            continue;
        }
        match decode_run(&run) {
            Some(text) if terminated => out.push_str(&text),
            _ => {
                out.push('&');
                out.push_str(&run);
                if terminated {
                    out.push('-');
                }
            }
        }
    }
    out
}

fn decode_run(run: &str) -> Option<String> {
    let bytes = modified_base64().decode(run).ok()?;
    if bytes.is_empty() || bytes.len() % 2 != 0 {
        return None;
    }
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
        .collect();
    String::from_utf16(&units).ok()
}

/// Turns text into a wire name. ASCII that needs no escaping stays byte for
/// byte identical, so a name that never left ASCII is untouched.
pub fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut run: Vec<u16> = Vec::new();
    for ch in value.chars() {
        match ch {
            '&' => {
                flush(&mut run, &mut out);
                out.push_str("&-");
            }
            '\u{20}'..='\u{7e}' => {
                flush(&mut run, &mut out);
                out.push(ch);
            }
            _ => {
                let mut buffer = [0u16; 2];
                run.extend_from_slice(ch.encode_utf16(&mut buffer));
            }
        }
    }
    flush(&mut run, &mut out);
    out
}

fn flush(run: &mut Vec<u16>, out: &mut String) {
    if run.is_empty() {
        return;
    }
    let mut bytes = Vec::with_capacity(run.len() * 2);
    for unit in run.drain(..) {
        bytes.extend_from_slice(&unit.to_be_bytes());
    }
    out.push('&');
    out.push_str(&modified_base64().encode(&bytes));
    out.push('-');
}

#[cfg(test)]
mod tests {
    use super::{decode, encode};

    #[test]
    fn plain_ascii_is_carried_through_untouched() {
        assert_eq!(decode("Work/2026"), "Work/2026");
        assert_eq!(encode("Work/2026"), "Work/2026");
        assert_eq!(encode("[Gmail]/All Mail"), "[Gmail]/All Mail");
    }

    #[test]
    fn turkish_folder_names_survive_a_round_trip() {
        for name in ["Önemli", "İş", "Faturalar/Şirket", "çğıöşü ÇĞİÖŞÜ"] {
            let wire = encode(name);
            assert!(wire.is_ascii(), "{wire} should be ASCII on the wire");
            assert_eq!(decode(&wire), name);
        }
    }

    #[test]
    fn decodes_what_a_server_actually_sends() {
        // The shapes RFC 3501 gives as examples, plus Gmail's own folder.
        assert_eq!(decode("&AO4-"), "î");
        assert_eq!(decode("~peter/mail/&U,BTFw-/&ZeVnLIqe-"), "~peter/mail/台北/日本語");
        assert_eq!(encode("~peter/mail/台北/日本語"), "~peter/mail/&U,BTFw-/&ZeVnLIqe-");
    }

    #[test]
    fn an_ampersand_is_escaped_both_ways() {
        assert_eq!(decode("Rock &- Roll"), "Rock & Roll");
        assert_eq!(encode("Rock & Roll"), "Rock &- Roll");
        assert_eq!(decode(&encode("A&B&-C")), "A&B&-C");
    }

    #[test]
    fn a_name_that_is_not_modified_utf7_is_returned_as_it_came() {
        // An unterminated run, and one that is not valid base64 at all.
        assert_eq!(decode("Sales & Marketing"), "Sales & Marketing");
        assert_eq!(decode("&not-base64-"), "&not-base64-");
    }
}

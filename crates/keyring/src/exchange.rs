//! gcr's `sx-aes-1` secret exchange: RFC 3526 1536-bit DH, HKDF-SHA256 and AES-128-CBC.

use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use cbc::cipher::block_padding::{NoPadding, Pkcs7};
use cbc::cipher::{BlockModeDecrypt, BlockModeEncrypt, KeyIvInit};
use crypto_bigint::modular::{FixedMontyForm, FixedMontyParams};
use crypto_bigint::{CtLt, Odd, U1536};
use zeroize::Zeroizing;

pub const PROTOCOL: &str = "sx-aes-1";
pub const MAX_TEXT: usize = 64 * 1024;
pub const MAX_SECRET: usize = 16 * 1024;

const BYTES: usize = 192;
const KEY: usize = 16;
const BLOCK: usize = 16;

/// RFC 3526, section 2: the 1536-bit MODP group. `egg-dh.c` holds the same bytes.
const PRIME_HEX: &str = concat!(
    "FFFFFFFFFFFFFFFFC90FDAA22168C234C4C6628B80DC1CD1",
    "29024E088A67CC74020BBEA63B139B22514A08798E3404DD",
    "EF9519B3CD3A431B302B0A6DF25F14374FE1356D6D51C245",
    "E485B576625E7EC6F44C42E9A637ED6B0BFF5CB6F406B7ED",
    "EE386BFB5A899FA5AE9F24117C4B1FE649286651ECE45B3D",
    "C2007CB8A163BF0598DA48361C55D39A69163FA8FD24CF5F",
    "83655D23DCA3AD961C62F356208552BB9ED529077096966D",
    "670C354E4ABC9804F1746C08CA237327FFFFFFFFFFFFFFFF",
);
const PRIME: Odd<U1536> = Odd::<U1536>::from_be_hex(PRIME_HEX);
const PARAMS: FixedMontyParams<{ U1536::LIMBS }> = FixedMontyParams::new_vartime(PRIME);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("no randomness: {0}")]
    Random(String),
    #[error("the text is not a {PROTOCOL} exchange")]
    Format,
    #[error("the peer sent no valid public value")]
    Public,
    #[error("a secret came before a key")]
    NoKey,
    #[error("the secret does not decrypt")]
    Decrypt,
    #[error("the secret is too long")]
    TooLong,
}

pub struct Exchange {
    private: Zeroizing<U1536>,
    public: Zeroizing<U1536>,
    key: Option<Zeroizing<[u8; KEY]>>,
}

impl fmt::Debug for Exchange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Exchange")
            .field("derived", &self.key.is_some())
            .finish_non_exhaustive()
    }
}

impl Exchange {
    pub fn new() -> Result<Self, Error> {
        let mut bytes = Zeroizing::new([0u8; BYTES]);
        loop {
            getrandom::fill(&mut bytes[..]).map_err(|error| Error::Random(error.to_string()))?;
            let private = Zeroizing::new(U1536::from_be_slice(&bytes[..]));
            let usable = private.ct_lt(PRIME.as_ref()) & !private.is_zero();
            if usable.to_bool() {
                return Ok(Self::with_private(private));
            }
        }
    }

    fn with_private(private: Zeroizing<U1536>) -> Self {
        let generator = FixedMontyForm::new(&U1536::from_u8(2), &PARAMS);
        let public = Zeroizing::new(generator.pow_amm(&private).retrieve());
        Self { private, public, key: None }
    }

    pub fn derived(&self) -> bool {
        self.key.is_some()
    }

    pub fn begin(&self) -> String {
        let mut text = String::with_capacity(300);
        self.write_public(&mut text);
        text
    }

    /// Only the first text with a `public` derives the key. Later ones are ignored.
    pub fn receive(&mut self, text: &str) -> Result<Option<Zeroizing<Vec<u8>>>, Error> {
        let fields = parse(text)?;
        if self.key.is_none() {
            let public = fields.public.ok_or(Error::Public)?;
            self.derive(public)?;
        }
        let Some(secret) = fields.secret else {
            return Ok(None);
        };
        let key = self.key.as_ref().ok_or(Error::NoKey)?;
        let iv = fields.iv.ok_or(Error::Decrypt)?;
        decrypt(key, &decode(iv).ok_or(Error::Decrypt)?, secret).map(Some)
    }

    pub fn send(&self, secret: Option<&[u8]>) -> Result<String, Error> {
        let mut iv = [0u8; BLOCK];
        if secret.is_some() {
            getrandom::fill(&mut iv).map_err(|error| Error::Random(error.to_string()))?;
        }
        self.send_with_iv(secret, &iv)
    }

    fn send_with_iv(&self, secret: Option<&[u8]>, iv: &[u8; BLOCK]) -> Result<String, Error> {
        let key = self.key.as_ref().ok_or(Error::NoKey)?;
        let mut text = String::with_capacity(300 + secret.map_or(0, |secret| secret.len() * 2));
        self.write_public(&mut text);
        if let Some(secret) = secret {
            if secret.len() > MAX_SECRET {
                return Err(Error::TooLong);
            }
            let ciphertext = encrypt(key, iv, secret)?;
            text.push_str("secret=");
            STANDARD.encode_string(&ciphertext, &mut text);
            text.push_str("\niv=");
            STANDARD.encode_string(iv, &mut text);
            text.push('\n');
        }
        Ok(text)
    }

    fn write_public(&self, text: &mut String) {
        let bytes = Zeroizing::new(self.public.to_be_bytes());
        text.push('[');
        text.push_str(PROTOCOL);
        text.push_str("]\npublic=");
        STANDARD.encode_string(minimal(&bytes), text);
        text.push('\n');
    }

    fn derive(&mut self, public: &str) -> Result<(), Error> {
        let raw = Zeroizing::new(decode(public).ok_or(Error::Public)?);
        // Any length, because gcr 4 sometimes sends 193 bytes with a leading zero.
        let digits = minimal(&raw);
        if digits.len() > BYTES {
            return Err(Error::Public);
        }
        let mut padded = Zeroizing::new([0u8; BYTES]);
        padded[BYTES - digits.len()..].copy_from_slice(digits);
        let peer = Zeroizing::new(U1536::from_be_slice(&padded[..]));
        let one = U1536::ONE;
        let top = PRIME.as_ref().wrapping_sub(&one);
        // The peer's value is public, so these comparisons may take variable time.
        if peer.cmp_vartime(&one).is_le() || peer.cmp_vartime(&top).is_ge() {
            return Err(Error::Public);
        }
        let base = Zeroizing::new(FixedMontyForm::new(&peer, &PARAMS));
        // `pow_amm` runs in constant time. Its own stack temporaries are not wiped.
        let power = Zeroizing::new(base.pow_amm(&self.private));
        let shared = Zeroizing::new(power.retrieve());
        let bytes = Zeroizing::new(shared.to_be_bytes());
        let mut key = Zeroizing::new([0u8; KEY]);
        // gcr hashes the shared secret without its leading zero bytes.
        hkdf::Hkdf::<sha2::Sha256>::new(None, minimal(&bytes))
            .expand(&[], &mut key[..])
            .map_err(|_| Error::Public)?;
        self.key = Some(key);
        Ok(())
    }
}

/// The bytes without their leading zeros, as gcr 3 writes a number.
fn minimal(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|byte| *byte != 0)
        .unwrap_or(bytes.len());
    &bytes[start..]
}

fn decode(text: &str) -> Option<Vec<u8>> {
    STANDARD.decode(text.trim()).ok()
}

fn encrypt(key: &[u8; KEY], iv: &[u8; BLOCK], secret: &[u8]) -> Result<Vec<u8>, Error> {
    let pad = BLOCK - secret.len() % BLOCK;
    let mut buffer = Zeroizing::new(Vec::with_capacity(secret.len() + pad));
    buffer.extend_from_slice(secret);
    buffer.resize(secret.len() + pad, pad as u8);
    let length = buffer.len();
    let cipher =
        cbc::Encryptor::<aes::Aes128>::new_from_slices(key, iv).map_err(|_| Error::NoKey)?;
    let ciphertext = cipher
        .encrypt_padded::<NoPadding>(&mut buffer, length)
        .map_err(|_| Error::TooLong)?;
    Ok(ciphertext.to_vec())
}

// gcr's format has no MAC, so none is checked.
fn decrypt(key: &[u8; KEY], iv: &[u8], secret: &str) -> Result<Zeroizing<Vec<u8>>, Error> {
    if iv.len() != BLOCK {
        return Err(Error::Decrypt);
    }
    let mut buffer = Zeroizing::new(decode(secret).ok_or(Error::Decrypt)?);
    if buffer.is_empty() || buffer.len() % BLOCK != 0 {
        return Err(Error::Decrypt);
    }
    let cipher =
        cbc::Decryptor::<aes::Aes128>::new_from_slices(key, iv).map_err(|_| Error::Decrypt)?;
    let plain = cipher
        .decrypt_padded::<Pkcs7>(&mut buffer)
        .map_err(|_| Error::Decrypt)?;
    let mut secret = Zeroizing::new(Vec::with_capacity(plain.len()));
    secret.extend_from_slice(plain);
    Ok(secret)
}

#[derive(Default)]
struct Fields<'a> {
    public: Option<&'a str>,
    secret: Option<&'a str>,
    iv: Option<&'a str>,
}

/// Reads the `[sx-aes-1]` group as `g_key_file_load_from_data` does for these keys.
fn parse(text: &str) -> Result<Fields<'_>, Error> {
    if text.len() > MAX_TEXT {
        return Err(Error::Format);
    }
    let mut fields = Fields::default();
    let mut group: Option<&str> = None;
    let mut seen = false;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
        {
            group = Some(name);
            seen |= name == PROTOCOL;
            continue;
        }
        let (key, value) = line
            .split_once('=')
            .ok_or(Error::Format)?;
        let Some(name) = group else {
            return Err(Error::Format);
        };
        if name != PROTOCOL {
            continue;
        }
        let value = value.trim();
        match key.trim() {
            "public" => fields.public = Some(value),
            "secret" => fields.secret = Some(value),
            "iv" => fields.iv = Some(value),
            _ => {}
        }
    }
    if !seen {
        return Err(Error::Format);
    }
    Ok(fields)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURES: &str = include_str!("../fixtures/exchange.txt");

    struct Case<'a> {
        name: &'a str,
        fields: Vec<(&'a str, &'a str)>,
    }

    impl Case<'_> {
        fn get(&self, key: &str) -> String {
            self.fields
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| value.replace('|', "\n"))
                .unwrap_or_else(|| panic!("{}: no {key}", self.name))
        }

        fn bytes(&self, key: &str) -> Vec<u8> {
            hex(&self.get(key))
        }
    }

    fn hex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&text[at..at + 2], 16).unwrap())
            .collect()
    }

    fn cases() -> Vec<Case<'static>> {
        let mut cases: Vec<Case<'static>> = Vec::new();
        for line in FIXTURES.lines() {
            if line.starts_with('#') || line.is_empty() {
                continue;
            }
            if let Some(name) = line
                .strip_prefix('[')
                .and_then(|rest| rest.strip_suffix(']'))
            {
                cases.push(Case { name, fields: Vec::new() });
            } else if let (Some(case), Some((key, value))) =
                (cases.last_mut(), line.split_once('='))
            {
                case.fields.push((key, value));
            }
        }
        cases
    }

    fn fixed(case: &Case<'_>) -> Exchange {
        let private = case.bytes("private");
        Exchange::with_private(Zeroizing::new(U1536::from_be_slice(&private)))
    }

    #[test]
    fn the_prime_is_rfc_3526() {
        // RFC 3526 writes p = 2^1536 - 2^1472 - 1 + 2^64 * (floor(2^1406 pi) + 741804). Its
        // top and bottom 64 bits are ones, and its hex in the RFC reads as below.
        let rfc = "FFFFFFFF FFFFFFFF C90FDAA2 2168C234 C4C6628B 80DC1CD1 \
            29024E08 8A67CC74 020BBEA6 3B139B22 514A0879 8E3404DD \
            EF9519B3 CD3A431B 302B0A6D F25F1437 4FE1356D 6D51C245 \
            E485B576 625E7EC6 F44C42E9 A637ED6B 0BFF5CB6 F406B7ED \
            EE386BFB 5A899FA5 AE9F2411 7C4B1FE6 49286651 ECE45B3D \
            C2007CB8 A163BF05 98DA4836 1C55D39A 69163FA8 FD24CF5F \
            83655D23 DCA3AD96 1C62F356 208552BB 9ED52907 7096966D \
            670C354E 4ABC9804 F1746C08 CA237327 FFFFFFFF FFFFFFFF";
        let rfc: String = rfc.split_whitespace().collect();
        assert_eq!(rfc, PRIME_HEX);
        let bytes = PRIME.as_ref().to_be_bytes();
        assert_eq!(bytes.len(), BYTES);
        assert!(
            bytes[..8]
                .iter()
                .all(|byte| *byte == 0xff)
        );
        assert!(
            bytes[BYTES - 8..]
                .iter()
                .all(|byte| *byte == 0xff)
        );
    }

    #[test]
    fn hkdf_matches_rfc_5869() {
        // RFC 5869, test case 3: SHA-256 with an empty salt and empty info.
        let ikm = [0x0bu8; 22];
        let mut okm = [0u8; 42];
        hkdf::Hkdf::<sha2::Sha256>::new(None, &ikm)
            .expand(&[], &mut okm)
            .unwrap();
        assert_eq!(
            okm.to_vec(),
            hex(concat!(
                "8da4e775a563c18f715f802a063c5a31b8a11f5c5ee1879ec3454e5f3c738d2d",
                "9d201395faa4b61a96c8"
            ))
        );
        // Test case 1, with salt and info, checks the same code with both set.
        let ikm = [0x0bu8; 22];
        let salt = hex("000102030405060708090a0b0c");
        let info = hex("f0f1f2f3f4f5f6f7f8f9");
        hkdf::Hkdf::<sha2::Sha256>::new(Some(&salt), &ikm)
            .expand(&info, &mut okm)
            .unwrap();
        assert_eq!(
            okm.to_vec(),
            hex(concat!(
                "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf",
                "34007208d5b887185865"
            ))
        );
    }

    #[test]
    fn the_fixtures_cover_every_case() {
        let names: Vec<&str> = cases()
            .iter()
            .map(|case| case.name)
            .collect();
        assert_eq!(
            names,
            ["gcr3", "gcr3-zero", "gcr4", "gcr4-zero", "gcr4-193", "gcr4-193-zero"]
        );
    }

    #[test]
    fn our_public_value_is_gcrs_text() {
        for case in cases() {
            assert_eq!(
                fixed(&case).begin(),
                case.get("ours_public"),
                "{}",
                case.name
            );
        }
    }

    /// gcr encrypted a secret to our public value, and we decrypt it.
    #[test]
    fn we_decrypt_what_gcr_sent() {
        for case in cases() {
            let mut ours = fixed(&case);
            let secret = ours
                .receive(&case.get("caller_send"))
                .unwrap_or_else(|error| panic!("{}: {error}", case.name))
                .expect("a secret");
            assert_eq!(*secret, case.bytes("secret"), "{}", case.name);

            let mut ours = fixed(&case);
            assert_eq!(ours.receive(&case.get("caller_begin")), Ok(None));
            assert!(ours.derived());
            let secret = ours
                .receive(&case.get("caller_send"))
                .unwrap()
                .unwrap();
            assert_eq!(*secret, case.bytes("secret"), "{}", case.name);
        }
    }

    /// We encrypt with the IV the fixture used, and gcr decrypted that exact text.
    #[test]
    fn gcr_decrypts_what_we_send() {
        for case in cases() {
            let mut ours = fixed(&case);
            ours.receive(&case.get("caller_begin"))
                .unwrap();
            let iv: [u8; BLOCK] = case.bytes("iv").try_into().unwrap();
            let sent = ours
                .send_with_iv(Some(&case.bytes("secret")), &iv)
                .unwrap();
            assert_eq!(sent, case.get("ours_send"), "{}", case.name);
        }
    }

    #[test]
    fn two_of_ours_agree() {
        for secret in [&b""[..], b"x", b"exactly sixteen!", "รหัสผ่าน".as_bytes()] {
            let mut caller = Exchange::new().unwrap();
            let mut prompter = Exchange::new().unwrap();
            assert_eq!(prompter.receive(&caller.begin()), Ok(None));
            assert_eq!(
                caller.receive(&prompter.send(None).unwrap()),
                Ok(None)
            );
            let sent = prompter.send(Some(secret)).unwrap();
            assert_eq!(
                caller
                    .receive(&sent)
                    .unwrap()
                    .as_deref()
                    .map(Vec::as_slice),
                Some(secret)
            );
        }
    }

    #[test]
    fn only_the_first_public_value_derives_the_key() {
        let mut caller = Exchange::new().unwrap();
        let other = Exchange::new().unwrap();
        let mut prompter = Exchange::new().unwrap();
        prompter
            .receive(&caller.begin())
            .unwrap();
        prompter
            .receive(&other.begin())
            .unwrap();
        caller
            .receive(&prompter.begin())
            .unwrap();
        let secret = caller
            .receive(&prompter.send(Some(b"pw")).unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(*secret, b"pw");
    }

    #[test]
    fn values_out_of_range_are_refused() {
        let p = PRIME.as_ref().to_be_bytes();
        let mut p_minus_1 = p.to_vec();
        p_minus_1[BYTES - 1] -= 1;
        let mut p_plus_1 = p.to_vec();
        p_plus_1[BYTES - 1] = 0;
        p_plus_1.insert(0, 1);
        let mut zero_padded_one = vec![0u8; 193];
        zero_padded_one[192] = 1;
        for (name, value) in [
            ("zero", vec![0u8]),
            ("empty", vec![]),
            ("one", vec![1u8]),
            ("one with zeros", zero_padded_one),
            ("p - 1", p_minus_1),
            ("p", p.to_vec()),
            ("over p", p_plus_1),
            ("too long", vec![0xff; 200]),
        ] {
            let text = format!("[sx-aes-1]\npublic={}\n", STANDARD.encode(&value));
            let mut ours = Exchange::new().unwrap();
            assert_eq!(ours.receive(&text), Err(Error::Public), "{name}");
            assert!(!ours.derived(), "{name}");
        }
        let mut two = Exchange::new().unwrap();
        assert_eq!(
            two.receive("[sx-aes-1]\npublic=Ag==\n"),
            Ok(None),
            "2 is in range"
        );
    }

    #[test]
    fn bad_texts_are_refused() {
        let mut ours = Exchange::new().unwrap();
        for text in [
            "",
            "public=AAA=\n",
            "[other]\npublic=Ag==\n",
            "[sx-aes-1]\njunk\n",
            "[sx-aes-1]\npublic=***\n",
            "[sx-aes-1]\n",
        ] {
            assert!(ours.receive(text).is_err(), "{text:?}");
        }
        let long = format!(
            "[sx-aes-1]\n#{}\npublic=Ag==\n",
            "x".repeat(MAX_TEXT)
        );
        assert_eq!(ours.receive(&long), Err(Error::Format));
        assert_eq!(
            ours.receive("# c\n\n[sx-aes-1]\n public = Ag== \n[x]\npublic=junk\n"),
            Ok(None),
            "comments, blank lines, spaces and other groups"
        );
        assert_eq!(
            ours.receive("[sx-aes-1]\nsecret=AAAAAAAAAAAAAAAAAAAAAA==\n"),
            Err(Error::Decrypt),
            "no IV"
        );
        assert_eq!(
            ours.receive("[sx-aes-1]\nsecret=AAAA\niv=AAAAAAAAAAAAAAAAAAAAAA==\n"),
            Err(Error::Decrypt),
            "not a whole block"
        );
    }

    #[test]
    fn a_secret_needs_a_key() {
        let ours = Exchange::new().unwrap();
        assert_eq!(ours.send(Some(b"pw")), Err(Error::NoKey));
        assert_eq!(ours.send(None), Err(Error::NoKey));
        assert!(
            ours.begin()
                .starts_with("[sx-aes-1]\npublic=")
        );
    }

    #[test]
    fn debug_shows_no_values() {
        let ours = Exchange::new().unwrap();
        let text = format!("{ours:?}");
        assert_eq!(text, "Exchange { derived: false, .. }");
    }
}

//! Agile encryption (MS-OFFCRYPTO 2.3.4.10 - 2.3.4.15): XML parameters,
//! a password-derived key that unlocks a random package key, 4096-byte
//! segments in AES-CBC, and an HMAC over the encrypted package.

use quick_xml::Reader as XmlReader;
use quick_xml::events::{BytesStart, Event};
use zeroize::Zeroizing;

use crate::detect::AgileSummary;
use crate::error::CryptoError;
use crate::util::{
    AES_BLOCK, Aes, HashAlgorithm, Reader, base64_decode, base64_encode, clean, ct_eq, fit,
    known_or_clean, random,
};
use crate::{Limits, Password, SaveKey, try_passwords};

const PASSWORD_URI: &str = "http://schemas.microsoft.com/office/2006/keyEncryptor/password";
const CERTIFICATE_URI: &str = "http://schemas.microsoft.com/office/2006/keyEncryptor/certificate";

// Block keys (2.3.4.11 - 2.3.4.14).
const BLOCK_VERIFIER_INPUT: [u8; 8] = [0xfe, 0xa7, 0xd2, 0x76, 0x3b, 0x4b, 0x9e, 0x79];
const BLOCK_VERIFIER_VALUE: [u8; 8] = [0xd7, 0xaa, 0x0f, 0x6d, 0x30, 0x61, 0x34, 0x4e];
const BLOCK_KEY_VALUE: [u8; 8] = [0x14, 0x6e, 0x0b, 0xe7, 0xab, 0xac, 0xd0, 0xd6];
const BLOCK_HMAC_KEY: [u8; 8] = [0x5f, 0xb2, 0xad, 0x01, 0x0c, 0xb9, 0xe1, 0xf6];
const BLOCK_HMAC_VALUE: [u8; 8] = [0xa0, 0x67, 0x7f, 0x02, 0xb2, 0x2c, 0x84, 0x33];

const SEGMENT: usize = 4096;
/// Largest salt accepted (the spec allows up to 65,536; Office writes 16).
const MAX_SALT: usize = 64;
/// Largest encrypted value accepted in the XML (keys, verifiers, HMACs).
const MAX_VALUE: usize = 1024;
/// Cipher, chaining and hash names from MS-OFFCRYPTO 2.3.4.10; anything
/// else from a file is cleaned before it is shown.
const CIPHERS: [&str; 7] = ["AES", "RC2", "RC4", "DES", "DESX", "3DES", "3DES_112"];
const CHAININGS: [&str; 2] = ["ChainingModeCBC", "ChainingModeCFB"];
const HASHES: [&str; 11] = [
    "SHA1",
    "SHA-1",
    "SHA256",
    "SHA384",
    "SHA512",
    "MD5",
    "MD4",
    "MD2",
    "RIPEMD-128",
    "RIPEMD-160",
    "WHIRLPOOL",
];
/// Longest quick-xml error text shown.
const MAX_XML_ERROR: usize = 120;
/// Excel's spin count; above it, the default password is not retried
/// after a wrong given password.
const RETRY_DEFAULT_MAX_SPIN: u32 = 100_000;
/// Most attributes on one element.
const MAX_ATTRIBUTES: usize = 64;

/// The cipher parameters shared by `keyData` and `p:encryptedKey`.
#[derive(Debug, Clone, Default)]
struct Params {
    salt: Vec<u8>,
    block_size: usize,
    key_bits: u32,
    hash_size: usize,
    cipher: String,
    chaining: String,
    hash: String,
}

#[derive(Debug, Clone, Default)]
struct PasswordKey {
    params: Params,
    spin_count: u32,
    verifier_input: Vec<u8>,
    verifier_value: Vec<u8>,
    key_value: Vec<u8>,
}

/// The parsed XML of agile `EncryptionInfo`.
#[derive(Debug, Clone)]
pub(crate) struct AgileInfo {
    key_data: Params,
    integrity: Option<(Vec<u8>, Vec<u8>)>,
    password: Option<PasswordKey>,
    certificate: bool,
}

/// What an element's attributes hold, read once.
struct Attrs(Vec<(String, String)>);

impl Attrs {
    fn read(e: &BytesStart<'_>) -> Result<Attrs, CryptoError> {
        let mut out = Vec::new();
        for a in e.attributes() {
            let a = a.map_err(|err| xml_error(clean(&err.to_string(), MAX_XML_ERROR)))?;
            if out.len() == MAX_ATTRIBUTES {
                return Err(xml_error(format!(
                    "an element has more than {MAX_ATTRIBUTES} attributes"
                )));
            }
            let key = a.key.local_name().as_ref().to_string();
            out.push((key, a.value.into_owned()));
        }
        Ok(Attrs(out))
    }

    fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    fn need(&self, name: &'static str) -> Result<&str, CryptoError> {
        self.get(name)
            .ok_or_else(|| xml_error(format!("the attribute `{name}` is missing")))
    }

    fn number(&self, name: &'static str) -> Result<u32, CryptoError> {
        self.need(name)?
            .trim()
            .parse::<u32>()
            .map_err(|_| xml_error(format!("the attribute `{name}` is not a number")))
    }

    fn bytes(&self, name: &'static str, max: usize) -> Result<Vec<u8>, CryptoError> {
        base64_decode(self.need(name)?, max, name)
    }

    fn params(&self) -> Result<Params, CryptoError> {
        let salt = self.bytes("saltValue", MAX_SALT)?;
        let salt_size = self.number("saltSize")? as usize;
        if salt.is_empty() || salt_size != salt.len() {
            return Err(xml_error(format!(
                "saltSize is {salt_size} but saltValue holds {} bytes",
                salt.len()
            )));
        }
        Ok(Params {
            salt,
            block_size: self.number("blockSize")? as usize,
            key_bits: self.number("keyBits")?,
            hash_size: self.number("hashSize")? as usize,
            cipher: known_or_clean(self.need("cipherAlgorithm")?, &CIPHERS),
            chaining: known_or_clean(self.need("cipherChaining")?, &CHAININGS),
            hash: known_or_clean(self.need("hashAlgorithm")?, &HASHES),
        })
    }
}

fn xml_error(detail: impl Into<String>) -> CryptoError {
    CryptoError::malformed("EncryptionInfo", detail)
}

/// Parses the agile XML with a size and event budget. quick-xml does not
/// expand entities from a DTD, and attribute values are taken raw, so
/// nothing in the XML can make it grow.
pub(crate) fn parse(xml: &[u8], limits: &Limits) -> Result<AgileInfo, CryptoError> {
    if xml.len() > limits.max_info_bytes {
        return Err(CryptoError::limit(
            "EncryptionInfo bytes",
            limits.max_info_bytes as u64,
        ));
    }
    let text = std::str::from_utf8(xml).map_err(|_| xml_error("the XML is not UTF-8"))?;
    let text = text.trim_start_matches('\u{feff}');
    let mut reader = XmlReader::from_str(text);
    let mut key_data = None;
    let mut integrity = None;
    let mut password = None;
    let mut certificate = false;
    let mut encryptor_uri: Option<String> = None;
    let mut events = 0usize;
    loop {
        events += 1;
        if events > limits.max_xml_events {
            return Err(CryptoError::limit(
                "EncryptionInfo XML events",
                limits.max_xml_events as u64,
            ));
        }
        let event = reader.read_event().map_err(|e| {
            xml_error(format!(
                "the XML is not well-formed: {}",
                clean(&e.to_string(), MAX_XML_ERROR)
            ))
        })?;
        let (e, is_start) = match &event {
            Event::Eof => break,
            Event::DocType(_) => {
                return Err(xml_error(
                    "the XML has a DOCTYPE, which Office never writes",
                ));
            }
            Event::Start(e) => (e, true),
            Event::Empty(e) => (e, false),
            Event::End(e) => {
                if e.local_name().as_ref() == "keyEncryptor" {
                    encryptor_uri = None;
                }
                continue;
            }
            _ => continue,
        };
        match e.local_name().as_ref() {
            "keyData" => key_data = Some(Attrs::read(e)?.params()?),
            "dataIntegrity" => {
                let a = Attrs::read(e)?;
                integrity = Some((
                    a.bytes("encryptedHmacKey", MAX_VALUE)?,
                    a.bytes("encryptedHmacValue", MAX_VALUE)?,
                ));
            }
            "keyEncryptor" => {
                let a = Attrs::read(e)?;
                let uri = a.need("uri")?.to_string();
                if uri == CERTIFICATE_URI {
                    certificate = true;
                }
                if is_start {
                    encryptor_uri = Some(uri);
                }
            }
            "encryptedKey"
                if encryptor_uri.as_deref() == Some(PASSWORD_URI) && password.is_none() =>
            {
                let a = Attrs::read(e)?;
                password = Some(PasswordKey {
                    params: a.params()?,
                    spin_count: a.number("spinCount")?,
                    verifier_input: a.bytes("encryptedVerifierHashInput", MAX_VALUE)?,
                    verifier_value: a.bytes("encryptedVerifierHashValue", MAX_VALUE)?,
                    key_value: a.bytes("encryptedKeyValue", MAX_VALUE)?,
                });
            }
            _ => {}
        }
    }
    let key_data = key_data.ok_or_else(|| xml_error("the keyData element is missing"))?;
    Ok(AgileInfo {
        key_data,
        integrity,
        password,
        certificate,
    })
}

/// Checks the cipher parameters and returns the hash and key length.
fn check(p: &Params, what: &str) -> Result<(HashAlgorithm, usize), CryptoError> {
    if p.cipher != "AES" {
        return Err(CryptoError::unsupported(format!(
            "agile encryption with the {} cipher",
            p.cipher
        )));
    }
    if p.chaining != "ChainingModeCBC" {
        return Err(CryptoError::unsupported(format!(
            "agile encryption with {} chaining",
            p.chaining
        )));
    }
    let hash = HashAlgorithm::from_name(&p.hash).ok_or_else(|| {
        CryptoError::unsupported(format!("agile encryption with the {} hash", p.hash))
    })?;
    if p.hash_size != hash.size() {
        return Err(xml_error(format!(
            "{what} says hashSize {} for {}, which is {} bytes",
            p.hash_size,
            p.hash,
            hash.size()
        )));
    }
    if p.block_size != AES_BLOCK {
        return Err(xml_error(format!(
            "{what} says blockSize {}; AES uses 16",
            p.block_size
        )));
    }
    if !matches!(p.key_bits, 128 | 192 | 256) {
        return Err(xml_error(format!(
            "{what} says keyBits {}; AES uses 128, 192 or 256",
            p.key_bits
        )));
    }
    Ok((hash, p.key_bits as usize / 8))
}

/// Decrypts `value` (whole AES blocks) and keeps the first `keep` bytes.
fn unwrap(
    aes: &Aes,
    iv: &[u8],
    value: &[u8],
    keep: usize,
    what: &str,
) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
    if !value.len().is_multiple_of(AES_BLOCK) || value.len() < keep {
        return Err(xml_error(format!(
            "{what} is {} bytes; it must be whole 16-byte blocks and at least {keep}",
            value.len()
        )));
    }
    let mut out = Zeroizing::new(value.to_vec());
    aes.cbc_decrypt(iv, &mut out);
    out.truncate(keep);
    Ok(out)
}

/// The IV for a value under the package key: H(keyData salt + block key).
fn data_iv(hash: HashAlgorithm, salt: &[u8], block: &[u8]) -> Zeroizing<Vec<u8>> {
    fit(&hash.digest(&[salt, block]), AES_BLOCK, 0x36)
}

impl AgileInfo {
    pub(crate) fn summary(&self) -> AgileSummary {
        let (spin_count, password) = match &self.password {
            Some(p) => (p.spin_count, true),
            None => (0, false),
        };
        AgileSummary {
            cipher: self.key_data.cipher.clone(),
            key_bits: self.key_data.key_bits,
            hash: self.key_data.hash.clone(),
            spin_count,
            password,
            certificate: self.certificate,
        }
    }

    /// Checks one password and returns the package key (2.3.4.13).
    fn package_key(
        &self,
        pk: &PasswordKey,
        password: &Password,
        package_key_len: usize,
    ) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
        let (hash, key_len) = check(&pk.params, "encryptedKey")?;
        let pw = password.utf16le()?;
        let h = hash.spin(&pk.params.salt, &pw, pk.spin_count);
        let iv = fit(&pk.params.salt, AES_BLOCK, 0x36);
        let block_aes = |block: &[u8]| -> Result<Aes, CryptoError> {
            let key = fit(&hash.digest(&[&h, block]), key_len, 0x36);
            Aes::new(&key)
        };
        let salt_len = pk.params.salt.len();
        let input = unwrap(
            &block_aes(&BLOCK_VERIFIER_INPUT)?,
            &iv,
            &pk.verifier_input,
            salt_len,
            "encryptedVerifierHashInput",
        )?;
        let value = unwrap(
            &block_aes(&BLOCK_VERIFIER_VALUE)?,
            &iv,
            &pk.verifier_value,
            hash.size(),
            "encryptedVerifierHashValue",
        )?;
        if !ct_eq(&hash.digest(&[&input]), &value) {
            return Err(CryptoError::WrongPassword);
        }
        unwrap(
            &block_aes(&BLOCK_KEY_VALUE)?,
            &iv,
            &pk.key_value,
            package_key_len,
            "encryptedKeyValue",
        )
    }

    /// Returns the package and whether its HMAC was checked (always, for
    /// agile: a file without `dataIntegrity` is refused, because anyone
    /// could have removed it to hide a change).
    pub(crate) fn decrypt(
        &self,
        package: &[u8],
        password: Option<&Password>,
        limits: &Limits,
    ) -> Result<((Vec<u8>, bool), SaveKey), CryptoError> {
        let (hash, key_len) = check(&self.key_data, "keyData")?;
        let Some(pk) = &self.password else {
            return Err(CryptoError::unsupported(
                "agile encryption that opens only with a certificate",
            ));
        };
        check(&pk.params, "encryptedKey")?;
        if pk.spin_count > limits.max_spin_count {
            return Err(CryptoError::limit(
                "spin count",
                u64::from(limits.max_spin_count),
            ));
        }
        let Some((enc_hmac_key, enc_hmac_value)) = &self.integrity else {
            return Err(CryptoError::IntegrityMissing);
        };

        // The declared size must be backed by data before any work.
        let mut r = Reader::new(package, "EncryptedPackage");
        let size = r.u64("the package size")?;
        let data = r.rest();
        let blocks = size
            .div_ceil(AES_BLOCK as u64)
            .saturating_mul(AES_BLOCK as u64);
        if blocks > data.len() as u64 {
            return Err(CryptoError::malformed(
                "EncryptedPackage",
                format!(
                    "it says the package is {size} bytes, but the stream holds only {} encrypted bytes",
                    data.len()
                ),
            ));
        }

        let retry_default = pk.spin_count <= RETRY_DEFAULT_MAX_SPIN;
        let (key, save) = try_passwords(password, retry_default, |p| {
            self.package_key(pk, p, key_len)
        })?;
        let save = save
            .with_spin_count(pk.spin_count, limits.max_spin_count)
            .with_certificate(self.certificate);
        let aes = Aes::new(&key)?;
        let salt = &self.key_data.salt;

        let iv = data_iv(hash, salt, &BLOCK_HMAC_KEY);
        let hmac_key = unwrap(&aes, &iv, enc_hmac_key, hash.size(), "encryptedHmacKey")?;
        let iv = data_iv(hash, salt, &BLOCK_HMAC_VALUE);
        let expected = unwrap(&aes, &iv, enc_hmac_value, hash.size(), "encryptedHmacValue")?;
        if !ct_eq(&hash.hmac(&hmac_key, package), &expected) {
            return Err(CryptoError::IntegrityFailed);
        }

        // `blocks <= data.len()`: the output never outgrows the real data.
        let mut out = data[..blocks as usize].to_vec();
        for (i, segment) in out.chunks_mut(SEGMENT).enumerate() {
            let index = u32::try_from(i)
                .map_err(|_| CryptoError::limit("package segments", u64::from(u32::MAX)))?;
            let iv = data_iv(hash, salt, &index.to_le_bytes());
            aes.cbc_decrypt(&iv, segment);
        }
        out.truncate(size as usize);
        Ok(((out, true), save))
    }
}

/// Encrypts a package; returns the `EncryptionInfo` and `EncryptedPackage`
/// stream bytes.
pub(crate) fn encrypt(
    package: &[u8],
    password: &Password,
    spin_count: u32,
) -> Result<(Vec<u8>, Vec<u8>), CryptoError> {
    let hash = HashAlgorithm::Sha512;
    let pw = password.utf16le()?;
    let key_salt = random(16)?;
    let pw_salt = random(16)?;
    let package_key = random(32)?;
    let verifier_input = random(16)?;
    let hmac_key = random(hash.size())?;

    // The password key encryptor.
    let h = hash.spin(&pw_salt, &pw, spin_count);
    let pw_iv = fit(&pw_salt, AES_BLOCK, 0x36);
    let wrap_pw = |block: &[u8], value: &[u8]| -> Result<Vec<u8>, CryptoError> {
        let key = fit(&hash.digest(&[&h, block]), 32, 0x36);
        let mut buf = pad_block(value);
        Aes::new(&key)?.cbc_encrypt(&pw_iv, &mut buf);
        Ok(buf.to_vec())
    };
    let enc_verifier_input = wrap_pw(&BLOCK_VERIFIER_INPUT, &verifier_input)?;
    let enc_verifier_value = wrap_pw(&BLOCK_VERIFIER_VALUE, &hash.digest(&[&verifier_input]))?;
    let enc_key_value = wrap_pw(&BLOCK_KEY_VALUE, &package_key)?;

    // The package, in 4096-byte segments.
    let aes = Aes::new(&package_key)?;
    let mut encrypted = Vec::with_capacity(8 + package.len() + AES_BLOCK);
    encrypted.extend_from_slice(&(package.len() as u64).to_le_bytes());
    for (i, segment) in package.chunks(SEGMENT).enumerate() {
        let index = u32::try_from(i)
            .map_err(|_| CryptoError::limit("package segments", u64::from(u32::MAX)))?;
        let mut buf = pad_block(segment);
        aes.cbc_encrypt(&data_iv(hash, &key_salt, &index.to_le_bytes()), &mut buf);
        encrypted.extend_from_slice(&buf);
    }

    // Integrity: an HMAC over the whole EncryptedPackage stream.
    let wrap_data = |block: &[u8], value: &[u8]| -> Vec<u8> {
        let mut buf = pad_block(value);
        aes.cbc_encrypt(&data_iv(hash, &key_salt, block), &mut buf);
        buf.to_vec()
    };
    let enc_hmac_key = wrap_data(&BLOCK_HMAC_KEY, &hmac_key);
    let enc_hmac_value = wrap_data(&BLOCK_HMAC_VALUE, &hash.hmac(&hmac_key, &encrypted));

    let params = |salt: &[u8]| {
        format!(
            r#"saltSize="16" blockSize="16" keyBits="256" hashSize="64" cipherAlgorithm="AES" cipherChaining="ChainingModeCBC" hashAlgorithm="SHA512" saltValue="{}""#,
            base64_encode(salt)
        )
    };
    let xml = format!(
        concat!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n",
            r#"<encryption xmlns="http://schemas.microsoft.com/office/2006/encryption" xmlns:p="{pw_uri}" xmlns:c="{cert_uri}">"#,
            r#"<keyData {key_params}/>"#,
            r#"<dataIntegrity encryptedHmacKey="{hmac_key}" encryptedHmacValue="{hmac_value}"/>"#,
            r#"<keyEncryptors><keyEncryptor uri="{pw_uri}">"#,
            r#"<p:encryptedKey spinCount="{spin}" {pw_params} encryptedVerifierHashInput="{vin}" encryptedVerifierHashValue="{vval}" encryptedKeyValue="{kval}"/>"#,
            r#"</keyEncryptor></keyEncryptors></encryption>"#
        ),
        pw_uri = PASSWORD_URI,
        cert_uri = CERTIFICATE_URI,
        key_params = params(&key_salt),
        hmac_key = base64_encode(&enc_hmac_key),
        hmac_value = base64_encode(&enc_hmac_value),
        spin = spin_count,
        pw_params = params(&pw_salt),
        vin = base64_encode(&enc_verifier_input),
        vval = base64_encode(&enc_verifier_value),
        kval = base64_encode(&enc_key_value),
    );
    let mut info = Vec::with_capacity(8 + xml.len());
    info.extend_from_slice(&4u16.to_le_bytes());
    info.extend_from_slice(&4u16.to_le_bytes());
    info.extend_from_slice(&0x40u32.to_le_bytes());
    info.extend_from_slice(xml.as_bytes());
    Ok((info, encrypted))
}

/// Copies `value` and pads it with zeros to whole AES blocks.
fn pad_block(value: &[u8]) -> Zeroizing<Vec<u8>> {
    let len = value.len().div_ceil(AES_BLOCK).max(1) * AES_BLOCK;
    fit(value, len, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn without_integrity(info: &[u8]) -> String {
        let xml = std::str::from_utf8(&info[8..]).unwrap();
        let start = xml.find("<dataIntegrity").unwrap();
        let end = start + xml[start..].find("/>").unwrap() + 2;
        format!("{}{}", &xml[..start], &xml[end..])
    }

    /// dataIntegrity is required: without it a change could go unseen.
    #[test]
    fn missing_data_integrity_is_refused() {
        let pw = Password::new("pw");
        let (info, package) = encrypt(b"tiny", &pw, 1).unwrap();
        let parsed = parse(&info[8..], &Limits::default()).unwrap();
        let ((plain, checked), _) = parsed
            .decrypt(&package, Some(&pw), &Limits::default())
            .unwrap();
        assert_eq!((plain.as_slice(), checked), (&b"tiny"[..], true));
        let stripped = parse(without_integrity(&info).as_bytes(), &Limits::default()).unwrap();
        let e = stripped
            .decrypt(&package, Some(&pw), &Limits::default())
            .unwrap_err();
        assert_eq!(e, CryptoError::IntegrityMissing);
    }

    /// A package size of 2^64-1 over a few bytes of data stops before any
    /// key stretching or allocation.
    #[test]
    fn declared_size_without_data_is_refused() {
        let pw = Password::new("pw");
        let (info, mut package) = encrypt(b"tiny", &pw, 1).unwrap();
        let parsed = parse(&info[8..], &Limits::default()).unwrap();
        package[..8].copy_from_slice(&u64::MAX.to_le_bytes());
        let e = parsed
            .decrypt(&package, Some(&pw), &Limits::default())
            .unwrap_err();
        assert!(matches!(
            e,
            CryptoError::Malformed {
                part: "EncryptedPackage",
                ..
            }
        ));
    }
}

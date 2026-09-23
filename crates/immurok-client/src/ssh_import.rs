//! ECDSA P-256 private key parsing for `KEY:IMPORT`. Two encodings:
//! OpenSSH (`-----BEGIN OPENSSH PRIVATE KEY-----`, unencrypted) and SEC1
//! PEM (`-----BEGIN EC PRIVATE KEY-----`). Pure functions; every failure is
//! an `Err(String)` the caller can show verbatim.

use base64::Engine;
use p256::elliptic_curve::sec1::ToEncodedPoint;
use p256::SecretKey;

fn pem_body_b64(pem_text: &str) -> Result<Vec<u8>, String> {
    let b64: String = pem_text.lines().filter(|l| !l.starts_with("-----")).collect::<Vec<_>>().join("");
    base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .map_err(|e| format!("Invalid base64 in key file: {}", e))
}

fn read_u32(data: &[u8], pos: &mut usize) -> Result<u32, String> {
    if *pos + 4 > data.len() {
        return Err("Truncated OpenSSH key (reading u32)".into());
    }
    let v = u32::from_be_bytes([data[*pos], data[*pos + 1], data[*pos + 2], data[*pos + 3]]);
    *pos += 4;
    Ok(v)
}

fn read_string(data: &[u8], pos: &mut usize) -> Result<Vec<u8>, String> {
    let len = read_u32(data, pos)? as usize;
    if *pos + len > data.len() {
        return Err("Truncated OpenSSH key (reading string)".into());
    }
    let s = data[*pos..*pos + len].to_vec();
    *pos += len;
    Ok(s)
}

/// OpenSSH private key → `(privkey_32_be, pubkey_64_be)`. Only
/// `ecdsa-sha2-nistp256`, unencrypted.
pub fn parse_openssh_key(pem_text: &str) -> Result<(Vec<u8>, Vec<u8>), String> {
    let data = pem_body_b64(pem_text)?;
    let magic = b"openssh-key-v1\0";
    if data.len() < magic.len() || &data[..magic.len()] != magic {
        return Err("Not a valid OpenSSH key (bad magic)".into());
    }
    let mut pos = magic.len();

    if read_string(&data, &mut pos)? != b"none" {
        return Err("Encrypted OpenSSH keys are not supported. Decrypt first with: ssh-keygen -p -f <keyfile>".into());
    }
    if read_string(&data, &mut pos)? != b"none" {
        return Err("Encrypted OpenSSH keys are not supported.".into());
    }
    let _kdf_opts = read_string(&data, &mut pos)?;
    let num_keys = read_u32(&data, &mut pos)?;
    if num_keys != 1 {
        return Err(format!("Expected 1 key, found {}", num_keys));
    }
    let _pubkey_blob = read_string(&data, &mut pos)?;
    let priv_section = read_string(&data, &mut pos)?;

    let mut p = 0;
    let check1 = read_u32(&priv_section, &mut p)?;
    let check2 = read_u32(&priv_section, &mut p)?;
    if check1 != check2 {
        return Err("OpenSSH key check values don't match (corrupted or encrypted?)".into());
    }
    let key_type = read_string(&priv_section, &mut p)?;
    if key_type != b"ecdsa-sha2-nistp256" {
        return Err(format!(
            "Unsupported key type '{}'. Only ecdsa-sha2-nistp256 is supported.",
            String::from_utf8_lossy(&key_type)
        ));
    }
    if read_string(&priv_section, &mut p)? != b"nistp256" {
        return Err("Unsupported curve. Only nistp256 (P-256) is supported.".into());
    }
    let pub_bytes = read_string(&priv_section, &mut p)?;
    if pub_bytes.len() != 65 || pub_bytes[0] != 0x04 {
        return Err(format!("Invalid public key length {} (expected 65 bytes uncompressed)", pub_bytes.len()));
    }
    let pubkey = pub_bytes[1..65].to_vec();

    let mut privkey = read_string(&priv_section, &mut p)?;
    if privkey.len() == 33 && privkey[0] == 0x00 {
        privkey.remove(0);
    }
    if privkey.len() != 32 {
        return Err(format!("Invalid private key length {} (expected 32 bytes)", privkey.len()));
    }
    Ok((privkey, pubkey))
}

fn der_tag_len(data: &[u8], pos: &mut usize) -> Result<(u8, usize), String> {
    if *pos + 2 > data.len() {
        return Err("Truncated DER data".into());
    }
    let tag = data[*pos];
    let len_byte = data[*pos + 1];
    *pos += 2;
    let len = if len_byte & 0x80 == 0 {
        len_byte as usize
    } else {
        let n = (len_byte & 0x7F) as usize;
        if *pos + n > data.len() {
            return Err("Truncated DER multi-byte length".into());
        }
        let mut l = 0usize;
        for i in 0..n {
            l = (l << 8) | data[*pos + i] as usize;
        }
        *pos += n;
        l
    };
    if len > data.len() - *pos {
        return Err("Truncated DER data (length exceeds buffer)".into());
    }
    Ok((tag, len))
}

/// SEC1 PEM (`EC PRIVATE KEY`) → `(privkey_32_be, pubkey_64_be)`. The
/// public key is derived from the private key when the file omits it.
pub fn parse_sec1_pem(pem_text: &str) -> Result<(Vec<u8>, Vec<u8>), String> {
    let der = pem_body_b64(pem_text)?;
    let mut pos = 0;

    let (tag, _) = der_tag_len(&der, &mut pos)?;
    if tag != 0x30 {
        return Err("Invalid DER: expected SEQUENCE".into());
    }
    let (tag, int_len) = der_tag_len(&der, &mut pos)?;
    if tag != 0x02 {
        return Err("Invalid DER: expected INTEGER (version)".into());
    }
    pos += int_len;
    let (tag, oct_len) = der_tag_len(&der, &mut pos)?;
    if tag != 0x04 {
        return Err("Invalid DER: expected OCTET STRING (private key)".into());
    }
    if oct_len != 32 || pos + 32 > der.len() {
        return Err(format!("Invalid private key length {} (expected 32)", oct_len));
    }
    let privkey = der[pos..pos + 32].to_vec();
    pos += 32;

    let mut pubkey: Option<Vec<u8>> = None;
    while pos < der.len() {
        let (tag, content_len) = der_tag_len(&der, &mut pos)?;
        if tag == 0xA1 && pos + content_len <= der.len() {
            let inner_start = pos;
            let (inner_tag, inner_len) = der_tag_len(&der, &mut pos)?;
            if inner_tag == 0x03 && inner_len >= 66 && pos + 66 <= der.len() && der[pos] == 0x00 && der[pos + 1] == 0x04 {
                pubkey = Some(der[pos + 2..pos + 66].to_vec());
            }
            pos = inner_start + content_len;
        } else {
            pos += content_len;
        }
    }

    let pubkey = match pubkey {
        Some(q) => q,
        None => derive_pubkey(&privkey)?,
    };
    Ok((privkey, pubkey))
}

fn derive_pubkey(privkey: &[u8]) -> Result<Vec<u8>, String> {
    let sk = SecretKey::from_slice(privkey).map_err(|e| format!("Invalid P-256 private key: {}", e))?;
    let point = sk.public_key().to_encoded_point(false);
    let bytes = point.as_bytes();
    if bytes.len() != 65 || bytes[0] != 0x04 {
        return Err("Failed to derive uncompressed public key".into());
    }
    Ok(bytes[1..65].to_vec())
}

/// `KEY:GENERATE` name payload: 16 bytes, NUL-padded, at most 15 bytes
/// copied — cut on a UTF-8 boundary so a multi-byte character right at the
/// edge is never split.
pub fn build_ssh_name_payload(name: &str) -> Vec<u8> {
    let mut buf = vec![0u8; 16];
    let truncated = crate::keys_import::truncate_utf8(name, 15);
    let nb = truncated.as_bytes();
    buf[..nb.len()].copy_from_slice(nb);
    buf
}

/// `KEY:IMPORT` payload: name[16] + pubkey_LE[64] + privkey_LE[32] = 112
/// bytes (the device stores all numbers little-endian). Verifies the
/// private key derives the embedded public key before packing.
pub fn build_ssh_import_payload(name: &str, pem_text: &str) -> Result<Vec<u8>, String> {
    let (privkey, pubkey) = if pem_text.contains("-----BEGIN OPENSSH PRIVATE KEY-----") {
        parse_openssh_key(pem_text)?
    } else if pem_text.contains("-----BEGIN EC PRIVATE KEY-----") {
        parse_sec1_pem(pem_text)?
    } else {
        return Err("Unsupported key format. Expected OpenSSH or SEC1 PEM (ECDSA P-256).".into());
    };

    if derive_pubkey(&privkey)? != pubkey {
        return Err("Public/private key pair mismatch — the keyfile's private key does not derive \
                    its embedded public key. Re-export with `ssh-keygen -y -f <privkey>` to check."
            .into());
    }

    let mut out = build_ssh_name_payload(name);
    let (x, y) = pubkey.split_at(32);
    out.extend(x.iter().rev());
    out.extend(y.iter().rev());
    out.extend(privkey.iter().rev());
    debug_assert_eq!(out.len(), 112);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const OPENSSH: &str = "-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAaAAAABNlY2RzYS
1zaGEyLW5pc3RwMjU2AAAACG5pc3RwMjU2AAAAQQRPPVQM5IubdgjP7hppZgl8ohYUtKmb
bMKL25uMuJDsf5H4AdzUVC97r2FyzB0/N+nUilcaxdmRIz767UYGZP4VAAAAoLfyPGK38j
xiAAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBE89VAzki5t2CM/u
GmlmCXyiFhS0qZtswovbm4y4kOx/kfgB3NRUL3uvYXLMHT836dSKVxrF2ZEjPvrtRgZk/h
UAAAAgSj+GHWttlSNlX/iCTDDA3aED/6v67geoseeej7jdNRkAAAAHZml4dHVyZQE=
-----END OPENSSH PRIVATE KEY-----
";

    const SEC1: &str = "-----BEGIN EC PRIVATE KEY-----
MHcCAQEEIEo/hh1rbZUjZV/4gkwwwN2hA/+r+u4HqLHnno+43TUZoAoGCCqGSM49
AwEHoUQDQgAETz1UDOSLm3YIz+4aaWYJfKIWFLSpm2zCi9ubjLiQ7H+R+AHc1FQv
e69hcswdPzfp1IpXGsXZkSM++u1GBmT+FQ==
-----END EC PRIVATE KEY-----
";

    const SEC1_NO_PUB: &str = "-----BEGIN EC PRIVATE KEY-----
MDECAQEEIEo/hh1rbZUjZV/4gkwwwN2hA/+r+u4HqLHnno+43TUZoAoGCCqGSM49
AwEH
-----END EC PRIVATE KEY-----
";

    #[test]
    fn both_encodings_parse_to_the_same_keypair() {
        let (p1, q1) = parse_openssh_key(OPENSSH).unwrap();
        let (p2, q2) = parse_sec1_pem(SEC1).unwrap();
        assert_eq!(p1, p2);
        assert_eq!(q1, q2);
        assert_eq!(p1.len(), 32);
        assert_eq!(q1.len(), 64);
        assert_eq!(&p1[..4], &[0x4a, 0x3f, 0x86, 0x1d]);
        assert_eq!(&q1[..4], &[0x4f, 0x3d, 0x54, 0x0c]);
        assert_eq!(&q1[61..], &[0x64, 0xfe, 0x15]);
    }

    #[test]
    fn import_payload_is_little_endian_112_bytes() {
        let p = build_ssh_import_payload("work", OPENSSH).unwrap();
        assert_eq!(p.len(), 112);
        assert_eq!(&p[..5], b"work\0");
        // pubkey.x LE: first byte is the last BE byte of x
        let (_, q) = parse_openssh_key(OPENSSH).unwrap();
        assert_eq!(p[16], q[31]);
        assert_eq!(p[16 + 63], q[32]); // y LE first byte at offset 48 … last at 79 = y[0]
        assert_eq!(p[80], 0x19); // priv LE first byte = last BE byte (…dd 35 19)
    }

    #[test]
    fn name_payload_truncates_to_15_bytes() {
        let n = build_ssh_name_payload("0123456789abcdefXYZ");
        assert_eq!(n.len(), 16);
        assert_eq!(&n[..15], b"0123456789abcde");
        assert_eq!(n[15], 0);
    }

    #[test]
    fn name_payload_truncates_on_utf8_boundary() {
        // "é" is 2 bytes; 10 repeats = 20 bytes. A raw 15-byte cut would
        // split the 8th "é" — the payload must stop at 14 bytes (7 complete
        // "é" chars) instead.
        let n = build_ssh_name_payload(&"é".repeat(10));
        assert_eq!(n.len(), 16);
        assert!(std::str::from_utf8(&n[..14]).is_ok());
        assert_eq!(std::str::from_utf8(&n[..14]).unwrap(), "é".repeat(7));
        assert_eq!(n[14], 0);
    }

    #[test]
    fn unsupported_inputs_give_specific_errors() {
        assert!(build_ssh_import_payload("x", "garbage").unwrap_err().contains("Unsupported key format"));
        assert!(parse_openssh_key("-----BEGIN OPENSSH PRIVATE KEY-----\nAAAA\n-----END OPENSSH PRIVATE KEY-----")
            .unwrap_err()
            .contains("bad magic"));
    }

    #[test]
    fn malformed_der_length_is_an_error() {
        let malformed_der = vec![0x30, 0x0a, 0x02, 0x88, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff];
        let pem = format!(
            "-----BEGIN EC PRIVATE KEY-----\n{}\n-----END EC PRIVATE KEY-----",
            base64::engine::general_purpose::STANDARD.encode(&malformed_der)
        );
        let result = parse_sec1_pem(&pem);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Truncated DER"));
    }

    #[test]
    fn sec1_without_public_key_derives_it() {
        let (priv_with_pub, pub_with_pub) = parse_sec1_pem(SEC1).unwrap();
        let (priv_no_pub, pub_no_pub) = parse_sec1_pem(SEC1_NO_PUB).unwrap();
        assert_eq!(priv_with_pub, priv_no_pub);
        assert_eq!(pub_with_pub, pub_no_pub);
    }
}

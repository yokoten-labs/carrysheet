//! Encryption: detection on committed samples, an agile `.xlsx` (100,000
//! rounds of key stretching), and RC4 decryption of a 26 MB `.xls` stream.

use carrysheet_crypto::{Limits, Password, decrypt_biff, decrypt_ooxml, detect};

use super::{Case, Input, Op, Operation, XLS_RC4, one, tally};
use crate::generate::XLS_PASSWORD;

pub fn cases() -> Vec<Case> {
    vec![
        Case {
            name: "crypto-samples/detect",
            op: Operation::Detect,
            about: "detect the protection of every committed sample",
            input: Input::Files {
                dirs: &["crates/carrysheet-crypto/tests/data"],
                exts: &["xls", "xlsx"],
            },
            inner: 1000,
            setup: |inputs| {
                Ok(Box::new(move || {
                    Ok(tally(
                        inputs.iter().map(|b| detect(b, &Limits::default())),
                        |p| u64::from(p.is_some()),
                    ))
                }))
            },
        },
        Case {
            name: "xlsx-agile/decrypt",
            op: Operation::Decrypt,
            about: "decrypt an agile .xlsx (100,000 SHA-1 rounds)",
            input: Input::Files {
                dirs: &["crates/carrysheet-crypto/tests/data/xlsx"],
                exts: &["xlsx"],
            },
            inner: 5,
            setup: |inputs| {
                // 60320-protected.xlsx is the sample with a known password.
                let file = inputs
                    .into_iter()
                    .find(|b| {
                        decrypt_ooxml(b, Some(&Password::new("Test001!!")), &Limits::default())
                            .is_ok()
                    })
                    .ok_or("no sample opens with the password Test001!!")?;
                Ok(Box::new(move || {
                    decrypt_ooxml(&file, Some(&Password::new("Test001!!")), &Limits::default())
                        .map(|d| d.package.len() as u64)
                        .map_err(|e| e.to_string())
                }))
            },
        },
        Case {
            name: "xls-1m-cells-rc4/decrypt",
            op: Operation::Decrypt,
            about: "decrypt the 1,048,576-cell workbook stream (RC4 CryptoAPI)",
            input: Input::Generated(&XLS_RC4),
            inner: 3,
            setup: decrypt_stream,
        },
    ]
}

fn decrypt_stream(inputs: Vec<Vec<u8>>) -> Result<Op, String> {
    let stream = one(inputs)?;
    Ok(Box::new(move || {
        decrypt_biff(
            &stream,
            Some(&Password::new(XLS_PASSWORD)),
            &Limits::default(),
        )
        .map(|d| d.stream.len() as u64)
        .map_err(|e| e.to_string())
    }))
}

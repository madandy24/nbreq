//! Diagnostic only: isolate Windows CryptoAPI chain-engine and SSL-policy calls.
//! No intentional persistent trust/config changes; CryptoAPI may retrieve/cache chain data.

use std::ffi::c_void;
use std::fs;
use std::io::{self, Write};
use std::mem::{offset_of, size_of};
use std::path::PathBuf;
use std::ptr::{null, null_mut};
use std::sync::Arc;

use rcgen::{
    BasicConstraints, CertificateParams, CertificateRevocationListParams, CustomExtension, DnType,
    ExtendedKeyUsagePurpose, IsCa, KeyIdMethod, KeyPair, KeyUsagePurpose, RevokedCertParams,
    SerialNumber,
};
use rustls::client::danger::ServerCertVerifier;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use serde_json::json;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use windows_sys::Win32::Foundation::{FILETIME, GetLastError, SetLastError};
use windows_sys::Win32::Security::Cryptography::{
    AUTHTYPE_SERVER, CERT_CHAIN_CACHE_END_CERT, CERT_CHAIN_CONTEXT, CERT_CHAIN_ENGINE_CONFIG,
    CERT_CHAIN_PARA, CERT_CHAIN_POLICY_IGNORE_ALL_REV_UNKNOWN_FLAGS, CERT_CHAIN_POLICY_PARA,
    CERT_CHAIN_POLICY_SSL, CERT_CHAIN_POLICY_STATUS, CERT_CHAIN_REVOCATION_ACCUMULATIVE_TIMEOUT,
    CERT_CHAIN_REVOCATION_CHECK_END_CERT, CERT_CONTEXT, CERT_STORE_ADD_ALWAYS,
    CERT_STORE_DEFER_CLOSE_UNTIL_LAST_FREE_FLAG, CERT_STORE_PROV_MEMORY, CertAddEncodedCRLToStore,
    CertAddEncodedCertificateToStore, CertCloseStore, CertCreateCertificateChainEngine,
    CertFreeCertificateChain, CertFreeCertificateChainEngine, CertFreeCertificateContext,
    CertGetCertificateChain, CertOpenStore, CertVerifyCertificateChainPolicy, HCERTCHAINENGINE,
    HCERTSTORE, HTTPSPolicyCallbackData, USAGE_MATCH_TYPE_AND, X509_ASN_ENCODING,
    szOID_PKIX_KP_SERVER_AUTH,
};

const CHAIN_FLAGS: u32 = CERT_CHAIN_REVOCATION_CHECK_END_CERT
    | CERT_CHAIN_REVOCATION_ACCUMULATIVE_TIMEOUT
    | CERT_CHAIN_CACHE_END_CERT;
const MAX_CERT_BYTES: usize = 64 * 1024;

struct Input {
    leaf: Vec<u8>,
    intermediates: Vec<Vec<u8>>,
    root: Option<Vec<u8>>,
    name: String,
    captured_at: Option<String>,
    evaluated_at: Option<i64>,
    compact_ca_key_usage: bool,
    crls: Option<CrlInputs>,
    before: Option<OffsetDateTime>,
    after: Option<OffsetDateTime>,
}

struct CrlInputs {
    empty: Vec<u8>,
    revoked: Vec<u8>,
    this_update: i64,
    next_update: i64,
    revocation_time: i64,
}

struct Store(HCERTSTORE);
impl Drop for Store {
    fn drop(&mut self) {
        unsafe {
            CertCloseStore(self.0, 0);
        }
    }
}

struct Cert(*mut CERT_CONTEXT);
impl Drop for Cert {
    fn drop(&mut self) {
        unsafe {
            CertFreeCertificateContext(self.0);
        }
    }
}

struct Engine(HCERTCHAINENGINE);
impl Drop for Engine {
    fn drop(&mut self) {
        unsafe {
            CertFreeCertificateChainEngine(self.0);
        }
    }
}

struct Chain(*mut CERT_CHAIN_CONTEXT);
impl Drop for Chain {
    fn drop(&mut self) {
        unsafe {
            CertFreeCertificateChain(self.0);
        }
    }
}

fn emit(value: serde_json::Value) {
    println!("{value}");
    let _ = io::stdout().flush();
}

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

// Pointers below belong to live CryptoAPI contexts. Bound traversal/output even
// when the system builds a longer chain or supplies unusually large extensions.
fn certificate_details(context: *const CERT_CONTEXT) -> serde_json::Value {
    if context.is_null() {
        return json!({"details_error":"null certificate context"});
    }
    let context = unsafe { &*context };
    if context.pCertInfo.is_null()
        || context.pbCertEncoded.is_null()
        || context.cbCertEncoded as usize > MAX_CERT_BYTES
    {
        return json!({"details_error":"certificate detail bound or null pointer"});
    }
    let der = unsafe {
        std::slice::from_raw_parts(context.pbCertEncoded, context.cbCertEncoded as usize)
    };
    let info = unsafe { &*context.pCertInfo };
    if info.cExtension != 0 && info.rgExtension.is_null() {
        return json!({"sha256":hash(der),"bytes":der.len(),
            "details_error":"null extension array","extension_count":info.cExtension});
    }
    let mut extensions = Vec::new();
    if info.cExtension <= 64 && (info.cExtension == 0 || !info.rgExtension.is_null()) {
        for index in 0..info.cExtension as usize {
            let extension = unsafe { &*info.rgExtension.add(index) };
            if extension.pszObjId.is_null()
                || extension.Value.cbData as usize > MAX_CERT_BYTES
                || (extension.Value.cbData != 0 && extension.Value.pbData.is_null())
            {
                extensions
                    .push(json!({"index":index,"details_error":"extension bound or null pointer"}));
                continue;
            }
            let mut oid_bytes = Vec::new();
            for index in 0..=128 {
                let byte = unsafe { *extension.pszObjId.add(index) };
                if byte == 0 {
                    break;
                }
                oid_bytes.push(byte);
            }
            if oid_bytes.len() > 128 || !oid_bytes.iter().all(|b| b.is_ascii_digit() || *b == b'.')
            {
                extensions.push(json!({"index":index,"details_error":"OID length bound"}));
                continue;
            }
            let oid = String::from_utf8(oid_bytes).expect("ASCII OID");
            let value = if extension.Value.cbData == 0 {
                &[]
            } else {
                unsafe {
                    std::slice::from_raw_parts(
                        extension.Value.pbData,
                        extension.Value.cbData as usize,
                    )
                }
            };
            let selected = matches!(oid.as_str(), "2.5.29.15" | "2.5.29.19" | "2.5.29.37");
            extensions.push(json!({"oid":oid,"critical":extension.fCritical != 0,
                "bytes":value.len(),"sha256":hash(value),
                "der_hex":if selected && value.len() <= 128 {
                    Some(value.iter().map(|b| format!("{b:02x}")).collect::<String>())
                } else { None }}));
        }
    }
    json!({"sha256":hash(der),"bytes":der.len(),"extension_count":info.cExtension,
        "extensions":extensions,"extensions_truncated":info.cExtension > 64})
}

fn chain_details(context: &Chain, label: &str) {
    let chain = unsafe { &*context.0 };
    if chain.cChain > 16 || (chain.cChain != 0 && chain.rgpChain.is_null()) {
        emit(
            json!({"event":"chain_details","label":label,"details_error":"simple chain bound or null pointer"}),
        );
        return;
    }
    for chain_index in 0..chain.cChain as usize {
        let simple = unsafe { *chain.rgpChain.add(chain_index) };
        if simple.is_null() {
            continue;
        }
        let simple = unsafe { &*simple };
        if simple.cElement > 16 || (simple.cElement != 0 && simple.rgpElement.is_null()) {
            emit(
                json!({"event":"chain_details","label":label,"chain_index":chain_index,
                "details_error":"element bound or null pointer"}),
            );
            continue;
        }
        for element_index in 0..simple.cElement as usize {
            let element = unsafe { *simple.rgpElement.add(element_index) };
            if element.is_null() {
                continue;
            }
            let element = unsafe { &*element };
            let revocation = if element.pRevocationInfo.is_null() {
                None
            } else {
                let revocation = unsafe { &*element.pRevocationInfo };
                Some(json!({"result":revocation.dwRevocationResult,
                    "has_freshness_time":revocation.fHasFreshnessTime != 0,
                    "freshness_time":revocation.dwFreshnessTime}))
            };
            emit(
                json!({"event":"chain_element","label":label,"chain_index":chain_index,
                "element_index":element_index,"trust_error_status":element.TrustStatus.dwErrorStatus,
                "trust_info_status":element.TrustStatus.dwInfoStatus,"revocation":revocation,
                "certificate":certificate_details(element.pCertContext)}),
            );
        }
    }
}

fn read_der(path: &PathBuf) -> Result<Vec<u8>, String> {
    let metadata = fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_CERT_BYTES as u64 {
        return Err(format!("DER input has invalid size: {}", path.display()));
    }
    fs::read(path).map_err(|e| format!("{}: {e}", path.display()))
}

fn input() -> Result<Input, String> {
    let options: Vec<_> = std::env::args().skip(1).collect();
    if options.is_empty() {
        return generated(false);
    }
    if options == ["--compact-ca-key-usage"] {
        return generated(true);
    }
    let mut args = options.into_iter();
    let mut leaf = None;
    let mut intermediates = Vec::new();
    let mut name = None;
    let mut captured_at = None;
    let mut evaluated_at = None;
    while let Some(flag) = args.next() {
        let value = args
            .next()
            .ok_or_else(|| format!("missing value for {flag}"))?;
        match flag.as_str() {
            "--leaf" if leaf.is_none() => leaf = Some(read_der(&PathBuf::from(value))?),
            "--intermediate" if intermediates.len() < 8 => {
                intermediates.push(read_der(&PathBuf::from(value))?)
            }
            "--name" if name.is_none() => name = Some(value),
            "--captured-at" if captured_at.is_none() => captured_at = Some(value),
            "--at-unix" if evaluated_at.is_none() => {
                let timestamp: i64 = value
                    .parse()
                    .map_err(|_| "--at-unix must be Unix seconds")?;
                // The pinned verifier multiplies Windows-epoch seconds by 1e9 in u64.
                if !(0..=6_802_270_473).contains(&timestamp) {
                    return Err("--at-unix is outside the pinned verifier's safe range".into());
                }
                evaluated_at = Some(timestamp);
            }
            _ => return Err(format!("unknown or repeated option: {flag}")),
        }
    }
    let name = name.ok_or("--name is required with --leaf")?;
    if name.len() > 253 || name.is_empty() || !name.is_ascii() || name.bytes().any(|b| b == 0) {
        return Err("--name must be a bounded ASCII DNS name".into());
    }
    let captured_at = captured_at.ok_or("--captured-at provenance is required with --leaf")?;
    if captured_at.len() > 128 || captured_at.is_empty() {
        return Err("--captured-at must be a short nonempty timestamp".into());
    }
    Ok(Input {
        leaf: leaf.ok_or("--leaf is required")?,
        intermediates,
        root: None,
        name,
        captured_at: Some(captured_at),
        evaluated_at,
        compact_ca_key_usage: false,
        crls: None,
        before: None,
        after: None,
    })
}

fn generated(compact_ca_key_usage: bool) -> Result<Input, String> {
    let now = OffsetDateTime::now_utc();
    let root_key = KeyPair::generate().map_err(|e| e.to_string())?;
    let mut root_params =
        CertificateParams::new(Vec::<String>::new()).map_err(|e| e.to_string())?;
    root_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    if compact_ca_key_usage {
        // Same keyCertSign+cRLSign bits, encoded without an unused trailing byte.
        let mut usage = CustomExtension::from_oid_content(&[2, 5, 29, 15], vec![3, 2, 1, 6]);
        usage.set_criticality(true);
        root_params.custom_extensions.push(usage);
    } else {
        root_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    }
    root_params
        .distinguished_name
        .push(DnType::CommonName, "Wine TLS diagnostic in-memory CA");
    let root = root_params
        .self_signed(&root_key)
        .map_err(|e| e.to_string())?;
    let leaf_key = KeyPair::generate().map_err(|e| e.to_string())?;
    let mut leaf_params = CertificateParams::new(vec!["fixture.example.test".to_owned()])
        .map_err(|e| e.to_string())?;
    leaf_params.not_before = now - time::Duration::days(1);
    leaf_params.not_after = now + time::Duration::days(30);
    if compact_ca_key_usage {
        leaf_params.serial_number = Some(SerialNumber::from(42_u64));
    }
    leaf_params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    leaf_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    leaf_params.use_authority_key_identifier_extension = true;
    leaf_params
        .distinguished_name
        .push(DnType::CommonName, "fixture.example.test");
    let before = leaf_params.not_before;
    let after = leaf_params.not_after;
    let leaf = leaf_params
        .signed_by(&leaf_key, &root, &root_key)
        .map_err(|e| e.to_string())?;
    let crls = if compact_ca_key_usage {
        let this_update = now - time::Duration::days(1);
        let next_update = now + time::Duration::days(90);
        let revocation_time = now - time::Duration::hours(1);
        let make_crl = |revoked| {
            CertificateRevocationListParams {
                this_update,
                next_update,
                crl_number: SerialNumber::from(if revoked { 2_u64 } else { 1_u64 }),
                issuing_distribution_point: None,
                revoked_certs: if revoked {
                    vec![RevokedCertParams {
                        serial_number: SerialNumber::from(42_u64),
                        revocation_time,
                        reason_code: None,
                        invalidity_date: None,
                    }]
                } else {
                    Vec::new()
                },
                key_identifier_method: KeyIdMethod::Sha256,
            }
            .signed_by(&root, &root_key)
            .map(|crl| crl.der().to_vec())
            .map_err(|e| e.to_string())
        };
        Some(CrlInputs {
            empty: make_crl(false)?,
            revoked: make_crl(true)?,
            this_update: this_update.unix_timestamp(),
            next_update: next_update.unix_timestamp(),
            revocation_time: revocation_time.unix_timestamp(),
        })
    } else {
        None
    };
    Ok(Input {
        leaf: leaf.der().to_vec(),
        intermediates: Vec::new(),
        root: Some(root.der().to_vec()),
        name: "fixture.example.test".into(),
        captured_at: None,
        evaluated_at: None,
        compact_ca_key_usage,
        crls,
        before: Some(before),
        after: Some(after),
    })
}

fn open_store(label: &str) -> Result<Store, String> {
    unsafe {
        SetLastError(0);
    }
    let handle = unsafe {
        CertOpenStore(
            CERT_STORE_PROV_MEMORY,
            0,
            0,
            CERT_STORE_DEFER_CLOSE_UNTIL_LAST_FREE_FLAG,
            null(),
        )
    };
    let error = if handle.is_null() {
        Some(unsafe { GetLastError() })
    } else {
        None
    };
    emit(
        json!({"event":"CertOpenStore", "label":label, "success":!handle.is_null(),
        "get_last_error_on_failure":error, "flags":CERT_STORE_DEFER_CLOSE_UNTIL_LAST_FREE_FLAG}),
    );
    if handle.is_null() {
        Err(format!("CertOpenStore {label} failed: {error:?}"))
    } else {
        Ok(Store(handle))
    }
}

fn add_cert(store: &Store, label: &str, der: &[u8]) -> Result<Cert, String> {
    let mut context = null_mut();
    unsafe {
        SetLastError(0);
    }
    let ok = unsafe {
        CertAddEncodedCertificateToStore(
            store.0,
            X509_ASN_ENCODING,
            der.as_ptr(),
            der.len() as u32,
            CERT_STORE_ADD_ALWAYS,
            &mut context,
        )
    };
    let error = if ok == 0 {
        Some(unsafe { GetLastError() })
    } else {
        None
    };
    emit(
        json!({"event":"CertAddEncodedCertificateToStore", "label":label,
        "success":ok != 0, "context_nonnull":!context.is_null(),
        "get_last_error_on_failure":error, "der_sha256":hash(der), "der_bytes":der.len()}),
    );
    if ok == 0 || context.is_null() {
        if !context.is_null() {
            unsafe {
                CertFreeCertificateContext(context);
            }
        }
        Err(format!(
            "CertAddEncodedCertificateToStore {label} failed: {error:?}"
        ))
    } else {
        emit(
            json!({"event":"certificate_details","label":label,"certificate":certificate_details(context)}),
        );
        Ok(Cert(context))
    }
}

fn engine(label: &str, size: u32, exclusive_root: HCERTSTORE) -> Option<Engine> {
    let mut config: CERT_CHAIN_ENGINE_CONFIG = unsafe { std::mem::zeroed() };
    config.cbSize = size;
    config.hExclusiveRoot = exclusive_root;
    let mut handle = null_mut();
    unsafe {
        SetLastError(0);
    }
    let ok = unsafe { CertCreateCertificateChainEngine(&config, &mut handle) };
    let error = if ok == 0 {
        Some(unsafe { GetLastError() })
    } else {
        None
    };
    emit(
        json!({"event":"CertCreateCertificateChainEngine", "label":label,
        "cb_size":size, "exclusive_root_nonnull":!exclusive_root.is_null(),
        "flags":config.dwFlags, "exclusive_flags":config.dwExclusiveFlags,
        "success":ok != 0, "engine_nonnull":!handle.is_null(),
        "get_last_error_on_failure":error}),
    );
    if ok != 0 && !handle.is_null() {
        Some(Engine(handle))
    } else {
        if !handle.is_null() {
            unsafe {
                CertFreeCertificateChainEngine(handle);
            }
        }
        None
    }
}

fn crl_controls(input: &Input, now: i64) -> Result<(), String> {
    let Some(crls) = &input.crls else {
        return Ok(());
    };
    let root = input.root.as_ref().ok_or("CRL fixture missing root")?;
    let expired_at = input
        .after
        .ok_or("CRL fixture missing expiry")?
        .unix_timestamp()
        + 86_400;
    for (variant, der, trust_root) in [
        ("empty", &crls.empty, true),
        ("revoked", &crls.revoked, true),
        ("untrusted", &crls.empty, false),
    ] {
        let label = format!("crl/{variant}");
        // Fresh stores and chain engines prevent a prior valid result being
        // reused for a different CRL. No CRL or key is persisted.
        let store = open_store(&format!("{label}/additional"))?;
        let leaf = add_cert(&store, &format!("{label}/leaf"), &input.leaf)?;
        let _root_in_additional = add_cert(&store, &format!("{label}/issuer"), root)?;
        let root_store = open_store(&format!("{label}/exclusive"))?;
        let _root_context = add_cert(&root_store, &format!("{label}/root"), root)?;
        unsafe {
            SetLastError(0);
        }
        let ok = unsafe {
            CertAddEncodedCRLToStore(
                store.0,
                X509_ASN_ENCODING,
                der.as_ptr(),
                der.len() as u32,
                CERT_STORE_ADD_ALWAYS,
                null_mut(),
            )
        };
        let error = if ok == 0 {
            Some(unsafe { GetLastError() })
        } else {
            None
        };
        emit(
            json!({"event":"CertAddEncodedCRLToStore","label":label,"success":ok != 0,
            "get_last_error_on_failure":error,"sha256":hash(der),"bytes":der.len(),
            "issuer_root_sha256":hash(root),"leaf_serial":42,"revoked_serial":if variant == "revoked" {Some(42)} else {None},
            "this_update_unix":crls.this_update,"next_update_unix":crls.next_update,
            "revocation_time_unix":if variant == "revoked" {Some(crls.revocation_time)} else {None}}),
        );
        if ok == 0 {
            continue;
        }
        let Some(engine) = engine(
            &label,
            offset_of!(CERT_CHAIN_ENGINE_CONFIG, dwExclusiveFlags) as u32,
            if trust_root { root_store.0 } else { null_mut() },
        ) else {
            continue;
        };
        let mut exact = None;
        let mut wrong = None;
        let mut expired = None;
        if let Some((chain, _)) = chain(
            &store,
            &leaf,
            Some(&engine),
            size_of::<CERT_CHAIN_PARA>() as u32,
            now,
            &label,
        )? {
            exact = policy(&chain, &format!("{label}/exact"), &input.name);
            wrong = policy(&chain, &format!("{label}/wrong_name"), "wrong.example.test");
        }
        if variant == "empty" {
            if let Some((chain, _)) = chain(
                &store,
                &leaf,
                Some(&engine),
                size_of::<CERT_CHAIN_PARA>() as u32,
                expired_at,
                &format!("{label}/expired"),
            )? {
                expired = policy(&chain, &format!("{label}/expired"), &input.name);
            }
        }
        let unsafe_acceptance =
            wrong == Some(0) || expired == Some(0) || (variant != "empty" && exact == Some(0));
        emit(
            json!({"event":"crl_control_summary","label":label,"exact_policy_error":exact,
            "wrong_name_policy_error":wrong,"expired_policy_error":expired,
            "positive_control_passed":variant == "empty" && exact == Some(0),
            "revoked_rejection_observed":variant == "revoked" && matches!(exact, Some(0x800b010c | 0x80092010)),
            "untrusted_root_rejection_observed":variant == "untrusted" && exact == Some(0x800b0109),
            "unsafe_negative_acceptance":unsafe_acceptance}),
        );
        if unsafe_acceptance {
            return Err(format!("invalid certificate accepted in {label}"));
        }
    }
    Ok(())
}

fn filetime(epoch: i64) -> Result<FILETIME, String> {
    let ticks = (i128::from(epoch) + 11_644_473_600_i128) * 10_000_000_i128;
    let ticks: u64 = ticks
        .try_into()
        .map_err(|_| "time outside FILETIME range")?;
    Ok(FILETIME {
        dwLowDateTime: ticks as u32,
        dwHighDateTime: (ticks >> 32) as u32,
    })
}

fn policy(chain: &Chain, label: &str, name: &str) -> Option<u32> {
    policy_flags(
        chain,
        label,
        name,
        CERT_CHAIN_POLICY_IGNORE_ALL_REV_UNKNOWN_FLAGS,
    )
}

fn policy_flags(chain: &Chain, label: &str, name: &str, flags: u32) -> Option<u32> {
    let mut wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    let mut extra: HTTPSPolicyCallbackData = unsafe { std::mem::zeroed() };
    extra.Anonymous.cbSize = size_of::<HTTPSPolicyCallbackData>() as u32;
    extra.dwAuthType = AUTHTYPE_SERVER;
    extra.pwszServerName = wide.as_mut_ptr();
    let mut params: CERT_CHAIN_POLICY_PARA = unsafe { std::mem::zeroed() };
    params.cbSize = size_of::<CERT_CHAIN_POLICY_PARA>() as u32;
    params.dwFlags = flags;
    params.pvExtraPolicyPara = (&mut extra as *mut HTTPSPolicyCallbackData).cast::<c_void>();
    let mut status: CERT_CHAIN_POLICY_STATUS = unsafe { std::mem::zeroed() };
    status.cbSize = size_of::<CERT_CHAIN_POLICY_STATUS>() as u32;
    unsafe {
        SetLastError(0);
    }
    let ok = unsafe {
        CertVerifyCertificateChainPolicy(CERT_CHAIN_POLICY_SSL, chain.0, &params, &mut status)
    };
    let error = if ok == 0 {
        Some(unsafe { GetLastError() })
    } else {
        None
    };
    emit(
        json!({"event":"CertVerifyCertificateChainPolicy", "label":label, "name":name,
        "success_call":ok != 0, "get_last_error_on_failure":error,
        "policy_flags":params.dwFlags, "policy_para_size":params.cbSize,
        "https_callback_size":size_of::<HTTPSPolicyCallbackData>(),
        "status_size":status.cbSize, "dw_error":if ok != 0 {Some(status.dwError)} else {None},
        "chain_index":if ok != 0 {Some(status.lChainIndex)} else {None},
        "element_index":if ok != 0 {Some(status.lElementIndex)} else {None}}),
    );
    if ok != 0 { Some(status.dwError) } else { None }
}

fn chain(
    store: &Store,
    cert: &Cert,
    engine: Option<&Engine>,
    para_size: u32,
    at: i64,
    label: &str,
) -> Result<Option<(Chain, u32)>, String> {
    let mut usage = [szOID_PKIX_KP_SERVER_AUTH as *mut u8];
    let mut params: CERT_CHAIN_PARA = unsafe { std::mem::zeroed() };
    params.cbSize = para_size;
    params.RequestedUsage.dwType = USAGE_MATCH_TYPE_AND;
    params.RequestedUsage.Usage.cUsageIdentifier = 1;
    params.RequestedUsage.Usage.rgpszUsageIdentifier = usage.as_mut_ptr();
    params.dwUrlRetrievalTimeout = 10_000;
    let when = filetime(at)?;
    let mut context = null_mut();
    unsafe {
        SetLastError(0);
    }
    let ok = unsafe {
        CertGetCertificateChain(
            engine.map_or(null_mut(), |e| e.0),
            cert.0,
            &when,
            store.0,
            &params,
            CHAIN_FLAGS,
            null(),
            &mut context,
        )
    };
    let error = if ok == 0 {
        Some(unsafe { GetLastError() })
    } else {
        None
    };
    let trust = if ok != 0 && !context.is_null() {
        Some(unsafe { (*context).TrustStatus.dwErrorStatus })
    } else {
        None
    };
    emit(
        json!({"event":"CertGetCertificateChain", "label":label, "at_unix":at,
        "engine_nonnull":engine.is_some(), "para_size":para_size,
        "url_timeout_ms":params.dwUrlRetrievalTimeout, "flags":CHAIN_FLAGS,
        "eku":"1.3.6.1.5.5.7.3.1", "success_call":ok != 0,
        "chain_nonnull":!context.is_null(), "get_last_error_on_failure":error,
        "trust_error_status":trust}),
    );
    if ok != 0 && !context.is_null() {
        let owned = Chain(context);
        chain_details(&owned, label);
        Ok(Some((owned, trust.unwrap())))
    } else {
        if !context.is_null() {
            unsafe {
                CertFreeCertificateChain(context);
            }
        }
        Ok(None)
    }
}

fn exercise(
    store: &Store,
    cert: &Cert,
    engine: Option<&Engine>,
    label: &str,
    input: &Input,
    now: i64,
) -> Result<(), String> {
    let current = size_of::<CERT_CHAIN_PARA>() as u32;
    let legacy = offset_of!(CERT_CHAIN_PARA, pStrongSignPara) as u32;
    for (tag, size) in [("current", current), ("pre_strong_sign", legacy)] {
        let case = format!("{label}/{tag}");
        let mut baseline_good = false;
        let mut wrong_name_error = None;
        let mut expired_error = None;
        let mut zero_flags_exact_error = None;
        let mut zero_flags_wrong_error = None;
        if let Some((valid, trust)) = chain(store, cert, engine, size, now, &case)? {
            let exact = policy(&valid, &format!("{case}/exact"), &input.name);
            // Policy may intentionally ignore revocation-unknown TrustStatus bits.
            baseline_good = exact == Some(0);
            wrong_name_error = policy(&valid, &format!("{case}/wrong_name"), "wrong.example.test");
            if input.root.is_none() {
                zero_flags_exact_error = policy_flags(
                    &valid,
                    &format!("{case}/zero_policy_flags/exact"),
                    &input.name,
                    0,
                );
                zero_flags_wrong_error = policy_flags(
                    &valid,
                    &format!("{case}/zero_policy_flags/wrong_name"),
                    "wrong.example.test",
                    0,
                );
            }
            emit(json!({"event":"baseline_trust", "label":case,
                "raw_trust_error_status":trust, "ssl_policy_accepted":baseline_good}));
        }
        if let Some(after) = input.after {
            let expired_at = after.unix_timestamp() + 86_400;
            if let Some((expired, _)) = chain(
                store,
                cert,
                engine,
                size,
                expired_at,
                &format!("{case}/expired"),
            )? {
                expired_error = policy(&expired, &format!("{case}/expired"), &input.name);
            }
        }
        let hostname_control_interpretable = baseline_good;
        let expiry_control_interpretable = baseline_good && input.after.is_some();
        let unsafe_acceptance = wrong_name_error == Some(0)
            || zero_flags_wrong_error == Some(0)
            || expired_error == Some(0)
            || (input.root.is_some() && engine.is_none() && baseline_good);
        emit(
            json!({"event":"control_summary", "label":case, "positive_control_passed":baseline_good,
            "hostname_control_interpretable":hostname_control_interpretable,
            "expiry_control_interpretable":expiry_control_interpretable,
            "wrong_name_policy_error":wrong_name_error,
            "expired_policy_error":expired_error,
            "zero_policy_flags_exact_error":zero_flags_exact_error,
            "zero_policy_flags_wrong_name_error":zero_flags_wrong_error,
            "unsafe_negative_acceptance":unsafe_acceptance}),
        );
        if unsafe_acceptance {
            return Err(format!("invalid certificate accepted in {case}"));
        }
    }
    Ok(())
}

fn dependency_verify(
    verifier: &rustls_platform_verifier::Verifier,
    input: &Input,
    label: &str,
    name: &str,
    at: i64,
) -> Result<bool, String> {
    let server_name = ServerName::try_from(name.to_owned()).map_err(|e| e.to_string())?;
    let leaf = CertificateDer::from(input.leaf.clone());
    let intermediate: Vec<_> = input
        .intermediates
        .iter()
        .cloned()
        .map(CertificateDer::from)
        .collect();
    let timestamp: u64 = at.try_into().map_err(|_| "negative verifier timestamp")?;
    let result = verifier.verify_server_cert(
        &leaf,
        &intermediate,
        &server_name,
        &[],
        UnixTime::since_unix_epoch(std::time::Duration::from_secs(timestamp)),
    );
    emit(
        json!({"event":"dependency_verify", "label":label, "name":name,
        "at_unix":at, "accepted":result.is_ok(), "result":format!("{result:?}")}),
    );
    Ok(result.is_ok())
}

fn dependency(input: &Input, now: i64) -> Result<(), String> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier =
        rustls_platform_verifier::Verifier::new(provider.clone()).map_err(|e| e.to_string())?;
    emit(
        json!({"event":"dependency_default_constructor", "result":"Ok", "note":"constructor success does not verify a server certificate"}),
    );
    let default_valid = dependency_verify(&verifier, input, "default/exact", &input.name, now)?;
    let default_wrong = dependency_verify(
        &verifier,
        input,
        "default/wrong_name",
        "wrong.example.test",
        now,
    )?;
    if default_wrong || (input.root.is_some() && default_valid) {
        return Err("dependency default verifier accepted invalid certificate".into());
    }
    if let Some(root) = &input.root {
        let result = rustls_platform_verifier::Verifier::new_with_extra_roots(
            vec![CertificateDer::from(root.clone())],
            provider,
        );
        emit(json!({"event":"dependency_extra_root_constructor", "result":format!("{result:?}")}));
        if let Ok(verifier) = result {
            let valid = dependency_verify(&verifier, input, "extra_root/exact", &input.name, now)?;
            let wrong = dependency_verify(
                &verifier,
                input,
                "extra_root/wrong_name",
                "wrong.example.test",
                now,
            )?;
            let after = input.after.ok_or("generated leaf missing validity end")?;
            let expired = dependency_verify(
                &verifier,
                input,
                "extra_root/expired",
                &input.name,
                after.unix_timestamp() + 86_400,
            )?;
            emit(
                json!({"event":"dependency_controls", "positive_control_passed":valid,
                "negative_controls_interpretable":valid, "wrong_name_accepted":wrong,
                "expired_accepted":expired}),
            );
            if wrong || expired {
                return Err("dependency extra-root verifier accepted invalid certificate".into());
            }
        }
    }
    Ok(())
}

fn run() -> Result<(), String> {
    let input = input()?;
    let now = OffsetDateTime::now_utc();
    let evaluated_at = input.evaluated_at.unwrap_or(now.unix_timestamp());
    emit(
        json!({"event":"input", "schema":1, "mode":if input.root.is_some() {"generated"} else {"public_chain"},
        "pointer_bits":usize::BITS, "evaluated_at_unix":evaluated_at,
        "observed_now_unix":now.unix_timestamp(),
        "captured_at":input.captured_at, "name":input.name,
        "compact_ca_key_usage":input.compact_ca_key_usage,
        "leaf_sha256":hash(&input.leaf),
        "intermediate_sha256":input.intermediates.iter().map(|c| hash(c)).collect::<Vec<_>>(),
        "root_sha256":input.root.as_ref().map(|c| hash(c)),
        "leaf_not_before_unix":input.before.map(|v| v.unix_timestamp()),
        "leaf_not_after_unix":input.after.map(|v| v.unix_timestamp()),
        "engine_config_current_size":size_of::<CERT_CHAIN_ENGINE_CONFIG>(),
        "engine_config_exclusive_root_offset":offset_of!(CERT_CHAIN_ENGINE_CONFIG, hExclusiveRoot),
        "engine_config_exclusive_flags_offset":offset_of!(CERT_CHAIN_ENGINE_CONFIG, dwExclusiveFlags),
        "chain_para_current_size":size_of::<CERT_CHAIN_PARA>(),
        "chain_para_strong_sign_offset":offset_of!(CERT_CHAIN_PARA, pStrongSignPara)}),
    );
    dependency(&input, evaluated_at)?;
    let store = open_store("leaf_and_intermediates")?;
    let leaf = add_cert(&store, "leaf", &input.leaf)?;
    let _intermediates: Vec<Cert> = input
        .intermediates
        .iter()
        .enumerate()
        .map(|(i, c)| add_cert(&store, &format!("intermediate_{i}"), c))
        .collect::<Result<_, _>>()?;
    exercise(&store, &leaf, None, "default_engine", &input, evaluated_at)?;
    if let Some(root) = &input.root {
        let root_store = open_store("exclusive_root")?;
        let _root_context = add_cert(&root_store, "root", root)?;
        let current = size_of::<CERT_CHAIN_ENGINE_CONFIG>() as u32;
        let pre_win8 = offset_of!(CERT_CHAIN_ENGINE_CONFIG, dwExclusiveFlags) as u32;
        let no_exclusive = offset_of!(CERT_CHAIN_ENGINE_CONFIG, hExclusiveRoot) as u32;
        for (label, size) in [
            ("current", current),
            ("pre_win8", pre_win8),
            ("no_exclusive_root_control", no_exclusive),
        ] {
            if let Some(root_engine) = engine(&format!("{label}/root"), size, root_store.0) {
                if size > no_exclusive {
                    exercise(
                        &store,
                        &leaf,
                        Some(&root_engine),
                        &format!("{label}/root"),
                        &input,
                        evaluated_at,
                    )?;
                } else {
                    emit(json!({"event":"constructor_only", "label":label,
                        "reason":"cbSize omits hExclusiveRoot; no extra-root trust claim"}));
                }
            }
            let _null_engine = engine(&format!("{label}/null_root_control"), size, null_mut());
        }
    }
    crl_controls(&input, evaluated_at)?;
    emit(json!({"event":"completed", "schema":1}));
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        emit(json!({"event":"fatal", "error":error}));
        std::process::exit(1);
    }
}

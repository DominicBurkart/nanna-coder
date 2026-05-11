//! Wire-format & security invariants for the env entity types
//! (`crate::entities::env::types`).
//!
//! These types are the persisted, queryable representation of container
//! configuration (issue #25). Two invariants are load-bearing for the
//! security posture and the cross-vendor serialization contract:
//!
//! 1. **No-plaintext-secret invariant.**
//!    [`EnvVarSource::SecretRef`] MUST persist only a `vault`/`key` locator
//!    pair. It MUST NOT serialize a `value` field, and a maliciously-crafted
//!    `SecretRef` JSON document with a `value` field appended MUST NOT
//!    silently round-trip as a `Literal`. A regression here would mean
//!    secret material could be smuggled into a config entity and end up on
//!    disk.
//!
//! 2. **Tag-discrimination invariant.**
//!    The two variants of [`EnvVarSource`] are discriminated by the `type`
//!    tag (`"literal"` vs `"secret_ref"`). Round-tripping each variant must
//!    deserialize back into the same arm, and the on-wire tag value is part
//!    of the public contract — clients in other languages depend on it.
//!
//! 3. **Restrictive-default invariant.**
//!    [`ContainerConfigEntity::new`] returns a value with a `SecurityContext`
//!    matching the restrictive default (read-only root, non-root, no caps,
//!    no allowed paths). Callers must explicitly opt in to relaxed posture.
//!
//! These invariants are exercised through the same public API that the rest
//! of the harness consumes (`serde_json`, the `Entity` trait, and the
//! `EnvVarSource` constructors) so a refactor that changes the on-wire format
//! will trip a test before it lands.
//!
//! Coverage rationale (qualitative): the in-module unit tests cover the
//! happy-path round-trip and a single negative assertion ("SecretRef must
//! not contain a `value` field"). They do not exercise the `Literal` arm's
//! round-trip, the `EnvVarSource::literal` constructor, the tag-name
//! contract, the `EnvEntity::new`/`Default` placeholder pair, or the
//! "tampered JSON" path where a `secret_ref` document grows a stray `value`
//! key. This file fills those gaps.

use harness::entities::env::security::{Capability, CapabilitySet, SecurityContext};
use harness::entities::env::types::{
    ContainerConfigEntity, EnvEntity, EnvVarRef, EnvVarSource, PortMapping, RuntimeConfig,
    VolumeMount,
};
use harness::entities::{Entity, EntityType};
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// EnvVarSource tag-discrimination and round-trip invariants
// ---------------------------------------------------------------------------

#[test]
fn env_var_source_literal_round_trip() {
    let src = EnvVarSource::literal("info");
    let json = serde_json::to_string(&src).expect("serialize Literal");
    let back: EnvVarSource = serde_json::from_str(&json).expect("deserialize Literal");
    match back {
        EnvVarSource::Literal { value } => assert_eq!(value, "info"),
        other => panic!("expected Literal, got {other:?}"),
    }
}

#[test]
fn env_var_source_secret_ref_round_trip() {
    let src = EnvVarSource::secret_ref("primary", "db_password");
    let json = serde_json::to_string(&src).expect("serialize SecretRef");
    let back: EnvVarSource = serde_json::from_str(&json).expect("deserialize SecretRef");
    match back {
        EnvVarSource::SecretRef { vault, key } => {
            assert_eq!(vault, "primary");
            assert_eq!(key, "db_password");
        }
        other => panic!("expected SecretRef, got {other:?}"),
    }
}

#[test]
fn env_var_source_literal_wire_tag_is_snake_case() {
    // The on-wire tag is part of the cross-vendor contract.
    let src = EnvVarSource::literal("x");
    let v: serde_json::Value = serde_json::to_value(&src).unwrap();
    assert_eq!(
        v.get("type").and_then(|t| t.as_str()),
        Some("literal"),
        "Literal must serialize with type=\"literal\": {v}"
    );
}

#[test]
fn env_var_source_secret_ref_wire_tag_is_snake_case() {
    let src = EnvVarSource::secret_ref("v", "k");
    let v: serde_json::Value = serde_json::to_value(&src).unwrap();
    assert_eq!(
        v.get("type").and_then(|t| t.as_str()),
        Some("secret_ref"),
        "SecretRef must serialize with type=\"secret_ref\": {v}"
    );
}

#[test]
fn env_var_source_unknown_tag_is_rejected() {
    // A typo in the discriminant must fail loudly, not silently fall back
    // to a default arm.
    let bogus = r#"{"type":"plaintext","value":"hi"}"#;
    let err = serde_json::from_str::<EnvVarSource>(bogus)
        .expect_err("unknown discriminant must fail to parse");
    let msg = err.to_string();
    assert!(
        msg.contains("plaintext") || msg.contains("variant"),
        "error should reference the unknown variant: {msg}"
    );
}

// ---------------------------------------------------------------------------
// No-plaintext-secret invariant
// ---------------------------------------------------------------------------

#[test]
fn secret_ref_with_smuggled_value_field_does_not_become_literal() {
    // Even if a document handed to us claims `type=secret_ref` but also
    // includes a `value` field, the deserializer must route it to the
    // SecretRef arm and discard the stray `value`. The reverse — a Literal
    // with an injected vault/key — must also stay a Literal.
    let tampered_secret = r#"{
        "type":"secret_ref",
        "vault":"primary",
        "key":"api_token",
        "value":"actual-plaintext-token-do-not-leak"
    }"#;
    let parsed: EnvVarSource =
        serde_json::from_str(tampered_secret).expect("must still parse as SecretRef");
    match &parsed {
        EnvVarSource::SecretRef { vault, key } => {
            assert_eq!(vault, "primary");
            assert_eq!(key, "api_token");
        }
        EnvVarSource::Literal { value } => panic!(
            "tampered SecretRef must NOT downgrade to Literal carrying {value:?} — \
             that would let secret material be smuggled into a persisted entity"
        ),
    }

    // Re-serializing the parsed form must drop the smuggled key entirely.
    let reserialized = serde_json::to_string(&parsed).unwrap();
    assert!(
        !reserialized.contains("actual-plaintext-token"),
        "re-serialized form must not contain the smuggled plaintext: {reserialized}"
    );
    assert!(
        !reserialized.contains("\"value\""),
        "SecretRef re-serialization must not introduce a value field: {reserialized}"
    );
}

#[test]
fn container_config_with_secret_env_never_serializes_plaintext() {
    let mut entity = ContainerConfigEntity::new("alpine:3.19".to_string());
    entity.env_refs.push(EnvVarRef {
        name: "DB_PASSWORD".to_string(),
        source: EnvVarSource::secret_ref("primary", "db_password"),
    });

    let json = entity.to_json().expect("serialize entity");
    // The vault and key identifiers are public; the secret value itself
    // is never present in this entity in the first place. The invariant
    // we're locking down: there is no `"value":` key emitted for the
    // SecretRef arm.
    let v: serde_json::Value = serde_json::from_str(&json).expect("re-parse");
    let env_refs = v
        .get("env_refs")
        .and_then(|x| x.as_array())
        .expect("env_refs array");
    let first = env_refs.first().expect("one env ref");
    let source = first.get("source").expect("source object");
    assert_eq!(
        source.get("type").and_then(|t| t.as_str()),
        Some("secret_ref")
    );
    assert!(
        source.get("value").is_none(),
        "SecretRef must not carry a `value` key: {source}"
    );
}

// ---------------------------------------------------------------------------
// Restrictive-default invariant
// ---------------------------------------------------------------------------

#[test]
fn container_config_new_starts_with_restrictive_security() {
    let entity = ContainerConfigEntity::new("alpine:3.19".to_string());
    let baseline = SecurityContext::default();

    assert!(
        entity.security.read_only_root_fs,
        "new() must set read_only_root_fs=true"
    );
    assert!(
        entity.security.run_as_non_root,
        "new() must set run_as_non_root=true"
    );
    assert_eq!(
        entity.security.capabilities,
        CapabilitySet::None,
        "new() must grant no capabilities"
    );
    assert!(
        entity.security.allowed_paths.is_empty(),
        "new() must start with no allowed paths"
    );
    assert_eq!(
        entity.security, baseline,
        "ContainerConfigEntity::new must match SecurityContext::default exactly"
    );

    // Runtime starts empty / unset.
    assert!(entity.runtime.cpu_limit.is_none());
    assert!(entity.runtime.memory_limit_mb.is_none());
    assert!(entity.runtime.ports.is_empty());
    assert!(entity.runtime.volumes.is_empty());
    assert!(entity.runtime.startup_timeout_secs.is_none());
    assert!(entity.runtime.health_check_timeout_secs.is_none());
    assert!(entity.env_refs.is_empty());

    // Image is preserved verbatim, no normalization.
    assert_eq!(entity.image, "alpine:3.19");

    // Entity type wiring is correct (it's a placeholder for issue #25 so this
    // is the only invariant we lock at this layer).
    assert_eq!(entity.metadata().entity_type, EntityType::Env);
}

// ---------------------------------------------------------------------------
// EnvEntity placeholder invariants
// ---------------------------------------------------------------------------

#[test]
fn env_entity_default_matches_new() {
    // EnvEntity is a placeholder (issue #25) but its Default and new()
    // constructors are part of the public API. We assert they produce
    // observationally equivalent metadata so future fields don't drift
    // between the two paths silently.
    let a = EnvEntity::new();
    let b = EnvEntity::default();
    assert_eq!(a.metadata().entity_type, EntityType::Env);
    assert_eq!(b.metadata().entity_type, EntityType::Env);
    assert_eq!(a.metadata().entity_type, b.metadata().entity_type);
}

#[test]
fn env_entity_serializes_to_valid_json() {
    let entity = EnvEntity::new();
    let json = entity.to_json().expect("EnvEntity must serialize");
    // The placeholder must at least produce a JSON object (not a bare
    // primitive) so it remains query-store-shaped.
    let v: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
    assert!(v.is_object(), "EnvEntity must serialize as object: {json}");
}

// ---------------------------------------------------------------------------
// Helper-type equality invariants
// ---------------------------------------------------------------------------

#[test]
fn port_mapping_eq_and_round_trip() {
    let pm = PortMapping {
        host: 8080,
        container: 80,
    };
    let pm2 = PortMapping {
        host: 8080,
        container: 80,
    };
    assert_eq!(pm, pm2, "PortMapping equality must be field-wise");

    let different = PortMapping {
        host: 9090,
        container: 80,
    };
    assert_ne!(pm, different);

    let json = serde_json::to_string(&pm).unwrap();
    let back: PortMapping = serde_json::from_str(&json).unwrap();
    assert_eq!(pm, back);
}

#[test]
fn volume_mount_read_only_flag_survives_round_trip() {
    let v = VolumeMount {
        host_path: PathBuf::from("/host/data"),
        container_path: PathBuf::from("/data"),
        read_only: true,
    };
    let json = serde_json::to_string(&v).unwrap();
    let back: VolumeMount = serde_json::from_str(&json).unwrap();
    assert_eq!(v, back, "VolumeMount must round-trip including read_only");
    assert!(back.read_only);
}

#[test]
fn runtime_config_default_is_all_unset() {
    let r = RuntimeConfig::default();
    assert!(r.cpu_limit.is_none());
    assert!(r.memory_limit_mb.is_none());
    assert!(r.ports.is_empty());
    assert!(r.volumes.is_empty());
    assert!(r.startup_timeout_secs.is_none());
    assert!(r.health_check_timeout_secs.is_none());
}

// ---------------------------------------------------------------------------
// SecurityContext / CapabilitySet integration with ContainerConfigEntity
// ---------------------------------------------------------------------------

#[test]
fn elevated_capabilities_survive_round_trip_via_container_entity() {
    // A caller that explicitly opts in to a Minimal capability set must see
    // that decision preserved through serde, not silently collapsed back to
    // None. This guards against a default-clobbering refactor of
    // SecurityContext.
    let mut entity = ContainerConfigEntity::new("alpine:3.19".to_string());
    entity.security.capabilities =
        CapabilitySet::Minimal(vec![Capability::NetBindService, Capability::Chown]);

    let json = entity.to_json().unwrap();
    let back: ContainerConfigEntity = serde_json::from_str(&json).unwrap();
    match back.security.capabilities {
        CapabilitySet::Minimal(caps) => {
            assert_eq!(caps, vec![Capability::NetBindService, Capability::Chown]);
        }
        other => panic!("expected Minimal, got {other:?}"),
    }
}

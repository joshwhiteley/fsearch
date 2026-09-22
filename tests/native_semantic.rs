//! Opt-in coverage of the real native runtime and model, never the fake embedder.
#![cfg(feature = "semantic")]

#[test]
#[ignore = "requires ONNX Runtime >= 1.24 and downloads the embedding model"]
fn native_model_produces_normalized_embeddings() {
    assert_ne!(std::env::var("FSEARCH_SEM_FAKE").ok().as_deref(), Some("1"));
    let before = std::env::var_os("ORT_DYLIB_PATH");
    let mut embedder = fsearch::sem::make_embedder().expect("load real native embedding model");
    assert_eq!(
        embedder.dim(),
        384,
        "must not exercise the 64-wide fake embedder"
    );
    let vectors = embedder
        .embed(&[
            "Find the project documentation".into(),
            "Locate the software user manual".into(),
        ])
        .expect("run native inference");
    assert_eq!(vectors.len(), 2);
    for vector in vectors {
        assert_eq!(vector.len(), 384);
        assert!(vector.iter().all(|value| value.is_finite()));
        let norm: f32 = vector.iter().map(|value| value * value).sum();
        assert!((norm - 1.0).abs() < 1e-4, "unexpected squared norm: {norm}");
    }
    assert_eq!(std::env::var_os("ORT_DYLIB_PATH"), before);
}

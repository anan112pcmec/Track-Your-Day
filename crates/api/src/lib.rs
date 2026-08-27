//! `api` — sengaja dikosongkan/minimal, isi terserah kebutuhan nanti
//! (REST server buat expose data ke web dashboard? gRPC? export JSON?).
//!
//! Satu-satunya "aturan main": kalau butuh baca data histori, panggil
//! lewat trait `separation::ActivityStore`, jangan depend langsung ke
//! crate `database`. Supaya storage tetap bisa diganti tanpa merusak API.

use separation::ActivityStore;
use std::sync::Arc;

/// Placeholder state. Nanti bisa jadi Router state (axum), dsb.
pub struct ApiState {
    pub store: Arc<dyn ActivityStore>,
}

impl ApiState {
    pub fn new(store: Arc<dyn ActivityStore>) -> Self {
        Self { store }
    }

    pub async fn health_check(&self) -> anyhow::Result<usize> {
        // Contoh minimal: hitung berapa event yang sudah tersimpan.
        Ok(self.store.recent(usize::MAX).await?.len())
    }
}

//! `api` — sengaja dikosongkan/minimal, isi terserah kebutuhan nanti
//! (REST server buat expose data ke web dashboard? gRPC? export JSON?).
//!
//! Satu-satunya "aturan main": kalau butuh baca data histori, panggil
//! lewat trait `separation::ActivityStore`, jangan depend langsung ke
//! crate `database`. Supaya storage tetap bisa diganti tanpa merusak API.

use anyhow::Ok;
use separation::{ActivityKind, ActivityStore, ActivityEvent};
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

    pub async fn wpm_check(&self) -> anyhow::Result<Option<u32>> {
        let events: Vec<ActivityEvent> = self.store.recent(200).await?;
        let latest_wpm = events.iter().rev().find_map(|event| match event.kind {
            ActivityKind::TypingSpeed { wpm } => Some(wpm),
            _ => None,
        });
        Ok(latest_wpm)
    }

    pub async fn running_application_check(&self) -> anyhow::Result<Option<String>> {
        let events: Vec<ActivityEvent> = self.store.recent(200).await?;
        let running_application: Option<String> = events.iter().rev().find_map(|event | match &event.kind {
            ActivityKind::ActiveWindow { app, title } => Some(app.clone()),
            _ => None,
        });
        Ok(running_application)
    }

    pub async fn window_id_check(&self) -> anyhow::Result<Option<usize>> {
        let events: Vec<ActivityEvent> = self.store.recent(200).await?;
        let window_id: Option<usize> = events.iter().rev().find_map(|event | match event.kind {
            ActivityKind::WindowId {id} => Some(id),
            _ => None,
        });
        return Ok(window_id)
    }
}

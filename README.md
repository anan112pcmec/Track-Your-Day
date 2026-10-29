# Track-YourDay

Monitor aktivitas laptop Windows langsung dari terminal — ringan, tanpa GUI berat, dan tanpa setup database. Dibangun dengan Rust dan DuckDB, tampilan memakai TUI (ratatui).

## Stack

<div align="center">
  <table>
    <tr>
      <td align="center" width="90">
        <img src="https://cdn.simpleicons.org/rust/CE412B" height="40" alt="rust logo" />
        <br/><sub><b>Rust</b></sub>
      </td>
      <td align="center" width="90">
        <img src="https://cdn.simpleicons.org/duckdb/FFF000" height="40" alt="duckdb logo" />
        <br/><sub><b>DuckDB</b></sub>
      </td>
    </tr>
  </table>
</div>

- **Rust** + **tokio** — runtime async untuk watcher
- **DuckDB** (embedded) — penyimpanan historical data dan query analytics
- **ratatui** + **crossterm** — antarmuka terminal
- **windows** (WinAPI) — pengambilan data CPU, RAM, Disk, dan WiFi
- **sysinfo**, **rdev**, **active-win-pos-rs** — jumlah proses, hook keyboard, dan window aktif

## Architecture
<img width="1168" height="1084" alt="image" src="https://github.com/user-attachments/assets/2bdad3f4-7a3c-48c2-b966-bed2f33db1e9" />

Project dibagi menjadi beberapa crate dalam satu Cargo workspace. Semua crate berkomunikasi lewat trait di crate `separation`, sehingga storage atau reactor bisa diganti tanpa mengubah crate lain.

| Crate | Peran |
|---|---|
| `cli` | Binary `track-your-day`. Merangkai semua komponen dan merender TUI |
| `watcher-mutator` | Polling data OS tiap 2 detik, lalu menyimpannya ke store dan mem-broadcast event |
| `watcher-reactive` | Mendengarkan broadcast dan menjalankan `ActivityReactor` (saat ini logger) |
| `database` | Implementasi `ActivityStore` di atas DuckDB (embedded) |
| `api` | Lapisan baca: mengambil event terbaru dan historical data dari store untuk UI |
| `separation` | Tipe data bersama (`ActivityEvent`, snapshot) dan trait `ActivityStore` / `ActivityReactor` |

## Flow

```
Track-YourDay (TUI · ratatui + crossterm)
    │   tab: Your Activity · Performance (CPU / Memory / Disk / WiFi)
    │   render tiap 100 ms
    ▼ baca data via ApiState
api
    │   ambil ActivityEvent lewat trait ActivityStore
    ▼
database (DuckDB · embedded)
    │   historical data · query analytics
    ▲ save()
    │
watcher-mutator (task tokio, polling tiap 2 detik)
    ├── Keyboard   (rdev hook → WPM)
    ├── Window     (active-win-pos-rs → app + title)
    ├── Process    (sysinfo → jumlah proses)
    ├── CPU        (WinAPI: utilisasi, clock, core, cache, uptime)
    ├── Memory     (WinAPI: in use, available, committed, cached, pool)
    ├── Disk       (WinAPI: kapasitas, read/write speed, active time)
    └── WiFi       (WinAPI: SSID, throughput, sinyal, IP)
    │
    ▼ broadcast channel
watcher-reactive
    └── ActivityReactor (saat ini LoggingReactor, verbose off)
```

## Fitur

- **Live monitoring** — CPU, RAM, Disk, dan WiFi ter-update berkala, lengkap dengan gauge, chart riwayat, dan panel detail
- **Your Activity** — kecepatan mengetik (WPM), aplikasi yang sedang aktif, jumlah proses, dan uptime
- **Historical data** — riwayat performa tersimpan di DuckDB, bisa di-query kapan saja
- **Embedded DB** — tidak perlu install database apa pun, DuckDB jalan langsung di dalam app
- **Ringan** — dibangun dengan Rust dan berjalan di terminal
- **Modular** — storage dan reactor bisa diganti lewat trait tanpa menyentuh crate lain

## Getting Started

### Prasyarat

- Windows
- [Rust toolchain](https://rustup.rs/)
- Jalankan terminal sebagai **Administrator** agar data Disk bisa terbaca (aplikasi membuka `\\.\PhysicalDrive0`)

### Build dan jalankan

```bash
git clone https://github.com/<your-org>/track-yourday.git
cd track-yourday

cargo run --release -p cli
```

### Kontrol

| Tombol | Aksi |
|---|---|
| `Tab` | Pindah antara tab *Your Activity* dan *Performance* |
| `↑` / `↓` | Pilih utilitas (CPU, Memory, Disk, WiFi) di tab *Performance* |
| `q` atau `Ctrl+C` | Keluar |

## Roadmap

- [ ] Process list lengkap dengan konsumsi resource per proses
- [ ] Monitoring jaringan selain WiFi, termasuk latency
- [ ] Dukungan Linux dan macOS
- [ ] Reactor tambahan (notifikasi desktop, ringkasan harian)
- [ ] Layer `api` untuk expose data (REST/JSON)

## License

GPL-3.0. Lihat [LICENSE](LICENSE).

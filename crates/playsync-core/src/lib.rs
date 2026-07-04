//! Logica compartilhada entre `playsyncd` (daemon) e `playsync` (CLI/TUI):
//! deteccao da Steam, protocolo de IPC, historico em sqlite e backends de nuvem.

pub mod archive;
pub mod cloud;
pub mod config;
pub mod db;
pub mod ipc;
pub mod manifest;
pub mod naming;
pub mod steam;
pub mod versions;

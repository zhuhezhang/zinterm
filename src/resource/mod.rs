#[path = "impls/system.rs"]
pub(crate) mod system;
#[path = "struct/system.rs"]
mod system_types;

#[cfg(target_os = "windows")]
pub(crate) use system_types::LocalGpuInfo;
pub(crate) use system_types::{
    LocalHardwareInfo, LocalSnap, NetHist, SystemSampler, SystemSnapshot, TabStatus, TabStatuses,
};

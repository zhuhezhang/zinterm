#[path = "impls/wallpaper.rs"]
#[allow(clippy::module_inception)]
mod wallpaper;
#[path = "struct/wallpaper.rs"]
mod wallpaper_types;

pub(crate) use wallpaper::*;

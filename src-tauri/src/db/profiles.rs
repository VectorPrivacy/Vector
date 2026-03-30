//! Profile database operations — delegates to vector-core.

pub use vector_core::SlimProfile;

pub async fn get_all_profiles() -> Result<Vec<SlimProfile>, String> {
    vector_core::db::profiles::get_all_profiles()
}

pub async fn set_profile(profile: SlimProfile) -> Result<(), String> {
    vector_core::db::profiles::set_profile(&profile)
}

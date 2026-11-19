use anyhow::Result;
use gpui::{Bounds, DisplayId, Pixels, PlatformDisplay, Size};
use std::time::Duration;
use uuid::Uuid;

/// The device's screen, as large as the activity's window: Android gives an app one.
#[derive(Debug)]
pub(crate) struct AndroidDisplay {
    pub size: Size<Pixels>,
    pub refresh_hz: f32,
}

impl PlatformDisplay for AndroidDisplay {
    fn id(&self) -> DisplayId {
        DisplayId::new(0)
    }

    fn uuid(&self) -> Result<Uuid> {
        Ok(Uuid::new_v5(&Uuid::NAMESPACE_OID, b"android-default-display"))
    }

    fn bounds(&self) -> Bounds<Pixels> {
        Bounds {
            origin: Default::default(),
            size: self.size,
        }
    }

    fn refresh_interval(&self) -> Option<Duration> {
        gpui::refresh_interval_from_hz(self.refresh_hz as f64)
    }
}

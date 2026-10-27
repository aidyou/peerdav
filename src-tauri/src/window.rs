//! Monitor-aware window preferences. No frontend mount or navigation participates
//! in restoration, so a webview reload cannot move or resize the native window.

use tauri::{Manager, PhysicalPosition, PhysicalSize, WebviewWindow, Window};

use crate::commands::SharedVault;
use crate::error::{AppError, DomainError};
use crate::types::{ERROR_EVENT, ScreenArea, WindowPreference};
use crate::vault::Vault;

const CONFIG_KEY: &str = "window.main";

fn native_error(error: tauri::Error) -> DomainError {
    DomainError::Internal {
        detail: error.to_string(),
    }
}

fn report(window: &Window, error: DomainError) {
    use tauri::Emitter;
    let error = AppError::from(error);
    if let Err(source) = window.emit(ERROR_EVENT, &error) {
        log::error!("failed to publish window preference error: {source}");
    }
}

/// Clamp the whole window into one work area, not just a single pixel of it.
/// Negative coordinates are valid on monitors left of the primary screen.
fn fit(
    preference: Option<&WindowPreference>,
    area: ScreenArea,
    keep_position: bool,
) -> (i32, i32, u32, u32) {
    let scale = if area.scale.is_finite() && area.scale > 0.0 {
        area.scale
    } else {
        1.0
    };
    let dimension = |value: f64, default: f64, available: u32| {
        let logical = if value.is_finite() && value >= 200.0 {
            value
        } else {
            default
        };
        (logical * scale)
            .round()
            .clamp(1.0, f64::from(available.max(1))) as u32
    };
    let width = dimension(preference.map_or(800.0, |p| p.width), 800.0, area.width);
    let height = dimension(preference.map_or(600.0, |p| p.height), 600.0, area.height);
    let max_x = i64::from(area.x) + i64::from(area.width.saturating_sub(width));
    let max_y = i64::from(area.y) + i64::from(area.height.saturating_sub(height));
    let centered_x = i64::from(area.x) + i64::from(area.width.saturating_sub(width) / 2);
    let centered_y = i64::from(area.y) + i64::from(area.height.saturating_sub(height) / 2);
    let x = preference
        .filter(|_| keep_position)
        .map_or(centered_x, |p| i64::from(p.x));
    let y = preference
        .filter(|_| keep_position)
        .map_or(centered_y, |p| i64::from(p.y));
    (
        x.clamp(i64::from(area.x), max_x) as i32,
        y.clamp(i64::from(area.y), max_y) as i32,
        width,
        height,
    )
}

pub(crate) fn restore(window: &WebviewWindow, vault: &Vault) -> Result<(), DomainError> {
    let preference = vault
        .get_config(CONFIG_KEY)?
        .map(|raw| {
            serde_json::from_str::<WindowPreference>(&raw).map_err(|source| {
                DomainError::Serialization {
                    operation: "read window preference",
                    source,
                }
            })
        })
        .transpose()?;
    let monitors = window.available_monitors().map_err(native_error)?;
    let matching = preference.as_ref().and_then(|saved| {
        saved
            .monitor
            .as_ref()
            .and_then(|name| monitors.iter().find(|m| m.name() == Some(name)))
    });
    let primary = window.primary_monitor().map_err(native_error)?;
    let Some(monitor) = matching.or(primary.as_ref()).or_else(|| monitors.first()) else {
        window.center().map_err(native_error)?;
        return Ok(());
    };
    let work = monitor.work_area();
    let area = ScreenArea {
        x: work.position.x,
        y: work.position.y,
        width: work.size.width,
        height: work.size.height,
        scale: monitor.scale_factor(),
    };
    let (x, y, width, height) = fit(preference.as_ref(), area, matching.is_some());
    window
        .set_size(PhysicalSize::new(width, height))
        .map_err(native_error)?;
    window
        .set_position(PhysicalPosition::new(x, y))
        .map_err(native_error)?;
    if preference.is_some_and(|p| p.maximized) {
        window.maximize().map_err(native_error)?;
    }
    Ok(())
}

pub(crate) fn save(window: &Window) {
    let Some(vault) = window.try_state::<SharedVault>() else {
        return;
    };
    if let Err(error) = save_inner(window, &vault) {
        report(window, error);
    }
}

fn save_inner(window: &Window, vault: &Vault) -> Result<(), DomainError> {
    if window.is_minimized().map_err(native_error)? {
        return Ok(());
    }
    let maximized = window.is_maximized().map_err(native_error)?;
    // Maximize events must not overwrite the user's normal restore bounds.
    let previous = vault
        .get_config(CONFIG_KEY)?
        .and_then(|raw| serde_json::from_str::<WindowPreference>(&raw).ok());
    let preference = if maximized {
        match previous {
            Some(mut saved) => {
                saved.maximized = true;
                saved
            }
            None => return Ok(()),
        }
    } else {
        let scale = window.scale_factor().map_err(native_error)?;
        let size = window
            .inner_size()
            .map_err(native_error)?
            .to_logical::<f64>(scale);
        if size.width < 200.0 || size.height < 200.0 {
            return Ok(());
        }
        let position = window.outer_position().map_err(native_error)?;
        let monitors = window.available_monitors().map_err(native_error)?;
        let monitor = monitors
            .iter()
            .find(|monitor| {
                let area = monitor.work_area();
                i64::from(position.x) >= i64::from(area.position.x)
                    && i64::from(position.y) >= i64::from(area.position.y)
                    && i64::from(position.x)
                        < i64::from(area.position.x) + i64::from(area.size.width)
                    && i64::from(position.y)
                        < i64::from(area.position.y) + i64::from(area.size.height)
            })
            .and_then(|monitor| monitor.name().cloned());
        WindowPreference {
            width: size.width,
            height: size.height,
            x: position.x,
            y: position.y,
            monitor,
            maximized,
        }
    };
    let encoded =
        serde_json::to_string(&preference).map_err(|source| DomainError::Serialization {
            operation: "save window preference",
            source,
        })?;
    vault.set_config(CONFIG_KEY, &encoded)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn preference(x: i32, y: i32) -> WindowPreference {
        WindowPreference {
            x,
            y,
            width: 800.0,
            height: 600.0,
            monitor: Some("removed".into()),
            maximized: false,
        }
    }
    fn area() -> ScreenArea {
        ScreenArea {
            x: 0,
            y: 0,
            width: 1920,
            height: 1040,
            scale: 1.0,
        }
    }
    #[test]
    fn first_launch_is_800_by_600_centered() {
        assert_eq!(fit(None, area(), false), (560, 220, 800, 600));
    }
    #[test]
    fn removed_screen_recenters_and_offscreen_positions_are_clamped() {
        assert_eq!(
            fit(Some(&preference(4000, 100)), area(), false),
            (560, 220, 800, 600)
        );
        assert_eq!(
            fit(Some(&preference(1900, 1000)), area(), true),
            (1120, 440, 800, 600)
        );
    }
    #[test]
    fn supports_negative_monitors_small_workareas_and_scale_changes() {
        let area = ScreenArea {
            x: -1280,
            y: 0,
            width: 1280,
            height: 720,
            scale: 2.0,
        };
        assert_eq!(
            fit(Some(&preference(-1100, 100)), area, true),
            (-1280, 0, 1280, 720)
        );
    }
    #[test]
    fn invalid_dimensions_fall_back_without_overflow() {
        let mut saved = preference(i32::MAX, i32::MIN);
        saved.width = f64::INFINITY;
        saved.height = -1.0;
        assert_eq!(fit(Some(&saved), area(), true), (1120, 0, 800, 600));
    }
}

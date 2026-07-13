// SPDX-License-Identifier: GPL-3.0-or-later

//! Display aspect classification shared by renderers and daemon transports.

use anyhow::{Result, bail};
use std::fmt;

/// Aspect class used by responsive layouts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DisplayShape {
    Portrait,
    Square,
    Landscape,
    Wide,
    Ultrawide,
}

impl DisplayShape {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Portrait => "portrait",
            Self::Square => "square",
            Self::Landscape => "landscape",
            Self::Wide => "wide",
            Self::Ultrawide => "ultrawide",
        }
    }
}

impl fmt::Display for DisplayShape {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Display shape class from native (or oriented) dimensions.
pub fn display_shape(width: u32, height: u32) -> Result<DisplayShape> {
    if width == 0 || height == 0 {
        bail!("invalid dimensions {width}x{height}");
    }
    if width < height {
        return Ok(DisplayShape::Portrait);
    }
    if width == height {
        return Ok(DisplayShape::Square);
    }

    let width = u64::from(width);
    let height = u64::from(height);
    let Some(width_100) = width.checked_mul(100) else {
        bail!("dimension overflow computing shape");
    };
    let Some(height_190) = height.checked_mul(190) else {
        bail!("dimension overflow computing shape");
    };
    let Some(height_275) = height.checked_mul(275) else {
        bail!("dimension overflow computing shape");
    };

    // Exact 1.9 / 2.75 ratios enter the higher class.
    if width_100 < height_190 {
        Ok(DisplayShape::Landscape)
    } else if width_100 < height_275 {
        Ok(DisplayShape::Wide)
    } else {
        Ok(DisplayShape::Ultrawide)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_boundaries_exactly() {
        assert_eq!(display_shape(240, 320).unwrap(), DisplayShape::Portrait);
        assert_eq!(display_shape(320, 320).unwrap(), DisplayShape::Square);
        assert_eq!(display_shape(854, 480).unwrap(), DisplayShape::Landscape);
        assert_eq!(display_shape(190, 100).unwrap(), DisplayShape::Wide);
        assert_eq!(display_shape(275, 100).unwrap(), DisplayShape::Ultrawide);
        assert!(display_shape(0, 480).is_err());
    }
}

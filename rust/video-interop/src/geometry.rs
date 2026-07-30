use crate::ValidationError;

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "rustler", derive(rustler::NifStruct))]
#[cfg_attr(feature = "rustler", module = "VideoInterop.Rect")]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    pub fn validate(&self, coded_width: u32, coded_height: u32) -> Result<(), ValidationError> {
        if coded_width == 0 || coded_height == 0 {
            return Err(ValidationError::ZeroCodedSize {
                width: coded_width,
                height: coded_height,
            });
        }
        if self.width == 0 || self.height == 0 {
            return Err(ValidationError::ZeroVisibleSize {
                width: self.width,
                height: self.height,
            });
        }

        let right = self
            .x
            .checked_add(self.width)
            .ok_or(ValidationError::VisibleRectOverflow)?;
        let bottom = self
            .y
            .checked_add(self.height)
            .ok_or(ValidationError::VisibleRectOverflow)?;

        if right > coded_width || bottom > coded_height {
            return Err(ValidationError::VisibleRectOutOfBounds {
                x: self.x,
                y: self.y,
                width: self.width,
                height: self.height,
                coded_width,
                coded_height,
            });
        }

        Ok(())
    }
}

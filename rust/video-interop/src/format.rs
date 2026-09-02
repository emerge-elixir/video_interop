use thiserror::Error;

/// Numeric validation errors for a stream format.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum FormatValidationError {
    #[error("stream format size must be positive, got {width}x{height}")]
    ZeroSize { width: u32, height: u32 },
    #[error("stream framerate must be positive, got {numerator}/{denominator}")]
    InvalidFramerate { numerator: u32, denominator: u32 },
    #[error("stream format has invalid DRM fourcc 0")]
    InvalidFourcc,
    #[error("BW1 binary format requires a polarity")]
    MissingBw1Polarity,
    #[error("BW1 polarity is only valid for BW1 binary format")]
    UnexpectedBw1Polarity,
    #[error("binary storage format requires implicit synchronization")]
    BinaryFormatRequiresImplicitSync,
    #[error("binary formats without alpha require opaque alpha mode")]
    BinaryFormatRequiresOpaqueAlpha,
    #[error("pixel aspect ratio must be positive, got {numerator}/{denominator}")]
    InvalidPixelAspectRatio { numerator: u32, denominator: u32 },
}

/// Positive rational represented as the Elixir `{numerator, denominator}` tuple.
pub type Rational = (u32, u32);

/// Synchronization policy declared by `VideoInterop.Format`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[cfg_attr(feature = "rustler", derive(rustler::NifUnitEnum))]
pub enum AcquireSyncPolicy {
    Implicit,
    SyncFile,
    PerFrame,
}

/// DMA-BUF modifier policy declared by `VideoInterop.DMABuf.Format`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ModifierPolicy {
    PerBuffer,
    Implicit,
    Explicit(u64),
}

/// Alias matching the stream-policy terminology used by native consumers.
pub type StreamAcquireSyncPolicy = AcquireSyncPolicy;
/// Alias matching the stream-policy terminology used by native consumers.
pub type StreamModifierPolicy = ModifierPolicy;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[cfg_attr(feature = "rustler", derive(rustler::NifUnitEnum))]
pub enum Primaries {
    Unspecified,
    Bt709,
    Bt470M,
    Bt470Bg,
    Smpte170m,
    Smpte240m,
    Film,
    Bt2020,
    Smpte428,
    Smpte431,
    Smpte432,
    Ebu3213,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[cfg_attr(feature = "rustler", derive(rustler::NifUnitEnum))]
pub enum Transfer {
    Unspecified,
    Bt709,
    Gamma22,
    Gamma28,
    Smpte170m,
    Smpte240m,
    Linear,
    Log,
    LogSqrt,
    Iec61966_2_4,
    Bt1361,
    Iec61966_2_1,
    Bt2020_10,
    Bt2020_12,
    Smpte2084,
    Smpte428,
    AribStdB67,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[cfg_attr(feature = "rustler", derive(rustler::NifUnitEnum))]
pub enum Matrix {
    Unspecified,
    Rgb,
    Bt709,
    Fcc,
    Bt470Bg,
    Smpte170m,
    Smpte240m,
    Ycgco,
    Bt2020Ncl,
    Bt2020Cl,
    Smpte2085,
    ChromaDerivedNcl,
    ChromaDerivedCl,
    Ictcp,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[cfg_attr(feature = "rustler", derive(rustler::NifUnitEnum))]
pub enum ColorRange {
    Unspecified,
    Limited,
    Full,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[cfg_attr(feature = "rustler", derive(rustler::NifUnitEnum))]
pub enum ChromaLocation {
    Unspecified,
    Left,
    Center,
    TopLeft,
    Top,
    BottomLeft,
    Bottom,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[cfg_attr(feature = "rustler", derive(rustler::NifUnitEnum))]
pub enum InterlaceMode {
    Progressive,
    InterlacedTopFirst,
    InterlacedBottomFirst,
    Mixed,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[cfg_attr(feature = "rustler", derive(rustler::NifUnitEnum))]
pub enum AlphaMode {
    Opaque,
    Straight,
    Premultiplied,
}

/// Rust representation of `VideoInterop.Colorimetry`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[cfg_attr(feature = "rustler", derive(rustler::NifStruct))]
#[cfg_attr(feature = "rustler", module = "VideoInterop.Colorimetry")]
pub struct Colorimetry {
    pub primaries: Primaries,
    pub transfer: Transfer,
    pub matrix: Matrix,
    pub range: ColorRange,
    pub chroma_location: ChromaLocation,
}

impl Default for Colorimetry {
    fn default() -> Self {
        Self {
            primaries: Primaries::Unspecified,
            transfer: Transfer::Unspecified,
            matrix: Matrix::Unspecified,
            range: ColorRange::Unspecified,
            chroma_location: ChromaLocation::Unspecified,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[cfg_attr(feature = "rustler", derive(rustler::NifUnitEnum))]
pub enum BinaryPixelFormat {
    Rgba8888,
    Rgb888,
    Gray8,
    Gray2,
    Bw1,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[cfg_attr(feature = "rustler", derive(rustler::NifUnitEnum))]
pub enum Bw1Polarity {
    OneIsBlack,
    OneIsWhite,
}

/// Rust representation of `VideoInterop.Binary.Format`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[cfg_attr(feature = "rustler", derive(rustler::NifStruct))]
#[cfg_attr(feature = "rustler", module = "VideoInterop.Binary.Format")]
pub struct BinaryFormat {
    pub pixel_format: BinaryPixelFormat,
    pub bw1_polarity: Option<Bw1Polarity>,
}

impl BinaryFormat {
    pub fn validate(&self) -> Result<(), FormatValidationError> {
        match (self.pixel_format, self.bw1_polarity) {
            (BinaryPixelFormat::Bw1, None) => Err(FormatValidationError::MissingBw1Polarity),
            (BinaryPixelFormat::Bw1, Some(_)) | (_, None) => Ok(()),
            (_, Some(_)) => Err(FormatValidationError::UnexpectedBw1Polarity),
        }
    }
}

/// Rust representation of `VideoInterop.DMABuf.Format`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[cfg_attr(feature = "rustler", derive(rustler::NifStruct))]
#[cfg_attr(feature = "rustler", module = "VideoInterop.DMABuf.Format")]
pub struct DmaBufFormat {
    pub fourcc: u32,
    pub modifier: ModifierPolicy,
}

impl DmaBufFormat {
    pub fn validate(&self) -> Result<(), FormatValidationError> {
        if self.fourcc == 0 {
            return Err(FormatValidationError::InvalidFourcc);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum StorageFormat {
    Binary(BinaryFormat),
    DmaBuf(DmaBufFormat),
}

impl StorageFormat {
    pub fn validate(&self) -> Result<(), FormatValidationError> {
        match self {
            Self::Binary(format) => format.validate(),
            Self::DmaBuf(format) => format.validate(),
        }
    }
}

/// Rust representation of the `VideoInterop.Format` stream schema.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
#[cfg_attr(feature = "rustler", derive(rustler::NifStruct))]
#[cfg_attr(feature = "rustler", module = "VideoInterop.Format")]
pub struct Format {
    pub width: u32,
    pub height: u32,
    pub framerate: Option<Rational>,
    pub storage: StorageFormat,
    pub acquire_sync: AcquireSyncPolicy,
    pub colorimetry: Colorimetry,
    pub pixel_aspect_ratio: Rational,
    pub interlace_mode: InterlaceMode,
    pub alpha_mode: AlphaMode,
}

impl Format {
    /// Validates the numeric invariants enforced by the Elixir stream schema.
    /// Enum and modifier-policy domains are enforced by their Rust types and
    /// Rustler decoders.
    pub fn validate(&self) -> Result<(), FormatValidationError> {
        if self.width == 0 || self.height == 0 {
            return Err(FormatValidationError::ZeroSize {
                width: self.width,
                height: self.height,
            });
        }

        if let Some((numerator, denominator)) = self.framerate
            && (numerator == 0 || denominator == 0)
        {
            return Err(FormatValidationError::InvalidFramerate {
                numerator,
                denominator,
            });
        }

        self.storage.validate()?;
        if let StorageFormat::Binary(storage) = self.storage {
            if self.acquire_sync != AcquireSyncPolicy::Implicit {
                return Err(FormatValidationError::BinaryFormatRequiresImplicitSync);
            }
            if storage.pixel_format != BinaryPixelFormat::Rgba8888
                && self.alpha_mode != AlphaMode::Opaque
            {
                return Err(FormatValidationError::BinaryFormatRequiresOpaqueAlpha);
            }
        }

        let (numerator, denominator) = self.pixel_aspect_ratio;
        if numerator == 0 || denominator == 0 {
            return Err(FormatValidationError::InvalidPixelAspectRatio {
                numerator,
                denominator,
            });
        }

        Ok(())
    }
}

#[cfg(feature = "rustler")]
mod rustler_impl {
    use rustler::{Decoder, Encoder, Env, Error, NifResult, Term};

    use super::{BinaryFormat, DmaBufFormat, ModifierPolicy, StorageFormat};

    mod atoms {
        rustler::atoms! {
            per_buffer,
            implicit
        }
    }

    impl<'a> Decoder<'a> for ModifierPolicy {
        fn decode(term: Term<'a>) -> NifResult<Self> {
            if let Ok(atom) = term.decode::<rustler::Atom>() {
                if atom == atoms::per_buffer() {
                    return Ok(Self::PerBuffer);
                }
                if atom == atoms::implicit() {
                    return Ok(Self::Implicit);
                }
                return Err(Error::BadArg);
            }

            term.decode::<u64>().map(Self::Explicit)
        }
    }

    impl<'a> Decoder<'a> for StorageFormat {
        fn decode(term: Term<'a>) -> NifResult<Self> {
            if let Ok(format) = term.decode::<DmaBufFormat>() {
                return Ok(Self::DmaBuf(format));
            }
            term.decode::<BinaryFormat>().map(Self::Binary)
        }
    }

    impl Encoder for StorageFormat {
        fn encode<'a>(&self, env: Env<'a>) -> Term<'a> {
            match self {
                Self::Binary(format) => format.encode(env),
                Self::DmaBuf(format) => format.encode(env),
            }
        }
    }

    impl Encoder for ModifierPolicy {
        fn encode<'a>(&self, env: Env<'a>) -> Term<'a> {
            match self {
                Self::PerBuffer => atoms::per_buffer().encode(env),
                Self::Implicit => atoms::implicit().encode(env),
                Self::Explicit(value) => value.encode(env),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn format() -> Format {
        Format {
            width: 640,
            height: 480,
            framerate: Some((60, 1)),
            storage: StorageFormat::DmaBuf(DmaBufFormat {
                fourcc: u32::from_le_bytes(*b"NV12"),
                modifier: ModifierPolicy::PerBuffer,
            }),
            acquire_sync: AcquireSyncPolicy::PerFrame,
            colorimetry: Colorimetry::default(),
            pixel_aspect_ratio: (1, 1),
            interlace_mode: InterlaceMode::Progressive,
            alpha_mode: AlphaMode::Opaque,
        }
    }

    #[test]
    fn validates_supported_stream_policy_values() {
        for modifier in [
            ModifierPolicy::PerBuffer,
            ModifierPolicy::Implicit,
            ModifierPolicy::Explicit(0),
            ModifierPolicy::Explicit(u64::MAX),
        ] {
            for acquire_sync in [
                AcquireSyncPolicy::Implicit,
                AcquireSyncPolicy::SyncFile,
                AcquireSyncPolicy::PerFrame,
            ] {
                let mut candidate = format();
                let StorageFormat::DmaBuf(storage) = &mut candidate.storage else {
                    unreachable!()
                };
                storage.modifier = modifier;
                candidate.acquire_sync = acquire_sync;
                assert_eq!(candidate.validate(), Ok(()));
            }
        }
    }

    #[test]
    fn binary_formats_require_implicit_synchronization() {
        let mut candidate = format();
        candidate.storage = StorageFormat::Binary(BinaryFormat {
            pixel_format: BinaryPixelFormat::Gray8,
            bw1_polarity: None,
        });
        assert_eq!(
            candidate.validate(),
            Err(FormatValidationError::BinaryFormatRequiresImplicitSync)
        );
        candidate.acquire_sync = AcquireSyncPolicy::Implicit;
        assert_eq!(candidate.validate(), Ok(()));
        candidate.alpha_mode = AlphaMode::Straight;
        assert_eq!(
            candidate.validate(),
            Err(FormatValidationError::BinaryFormatRequiresOpaqueAlpha)
        );
    }

    #[test]
    fn default_colorimetry_is_explicitly_unspecified() {
        assert_eq!(
            Colorimetry::default(),
            Colorimetry {
                primaries: Primaries::Unspecified,
                transfer: Transfer::Unspecified,
                matrix: Matrix::Unspecified,
                range: ColorRange::Unspecified,
                chroma_location: ChromaLocation::Unspecified,
            }
        );
    }

    #[test]
    fn represents_every_elixir_colorimetry_value() {
        let primaries = [
            Primaries::Unspecified,
            Primaries::Bt709,
            Primaries::Bt470M,
            Primaries::Bt470Bg,
            Primaries::Smpte170m,
            Primaries::Smpte240m,
            Primaries::Film,
            Primaries::Bt2020,
            Primaries::Smpte428,
            Primaries::Smpte431,
            Primaries::Smpte432,
            Primaries::Ebu3213,
        ];
        let transfers = [
            Transfer::Unspecified,
            Transfer::Bt709,
            Transfer::Gamma22,
            Transfer::Gamma28,
            Transfer::Smpte170m,
            Transfer::Smpte240m,
            Transfer::Linear,
            Transfer::Log,
            Transfer::LogSqrt,
            Transfer::Iec61966_2_4,
            Transfer::Bt1361,
            Transfer::Iec61966_2_1,
            Transfer::Bt2020_10,
            Transfer::Bt2020_12,
            Transfer::Smpte2084,
            Transfer::Smpte428,
            Transfer::AribStdB67,
        ];
        let matrices = [
            Matrix::Unspecified,
            Matrix::Rgb,
            Matrix::Bt709,
            Matrix::Fcc,
            Matrix::Bt470Bg,
            Matrix::Smpte170m,
            Matrix::Smpte240m,
            Matrix::Ycgco,
            Matrix::Bt2020Ncl,
            Matrix::Bt2020Cl,
            Matrix::Smpte2085,
            Matrix::ChromaDerivedNcl,
            Matrix::ChromaDerivedCl,
            Matrix::Ictcp,
        ];
        let ranges = [
            ColorRange::Unspecified,
            ColorRange::Limited,
            ColorRange::Full,
        ];
        let chroma_locations = [
            ChromaLocation::Unspecified,
            ChromaLocation::Left,
            ChromaLocation::Center,
            ChromaLocation::TopLeft,
            ChromaLocation::Top,
            ChromaLocation::BottomLeft,
            ChromaLocation::Bottom,
        ];

        assert_eq!(primaries.len(), 12);
        assert_eq!(transfers.len(), 17);
        assert_eq!(matrices.len(), 14);
        assert_eq!(ranges.len(), 3);
        assert_eq!(chroma_locations.len(), 7);
    }

    #[test]
    fn rejects_invalid_numeric_schema_values() {
        let mut candidate = format();
        candidate.width = 0;
        assert_eq!(
            candidate.validate(),
            Err(FormatValidationError::ZeroSize {
                width: 0,
                height: 480
            })
        );

        let mut candidate = format();
        candidate.framerate = Some((60, 0));
        assert_eq!(
            candidate.validate(),
            Err(FormatValidationError::InvalidFramerate {
                numerator: 60,
                denominator: 0
            })
        );

        let mut candidate = format();
        let StorageFormat::DmaBuf(storage) = &mut candidate.storage else {
            unreachable!()
        };
        storage.fourcc = 0;
        assert_eq!(
            candidate.validate(),
            Err(FormatValidationError::InvalidFourcc)
        );

        let mut candidate = format();
        candidate.pixel_aspect_ratio = (0, 1);
        assert_eq!(
            candidate.validate(),
            Err(FormatValidationError::InvalidPixelAspectRatio {
                numerator: 0,
                denominator: 1
            })
        );
    }
}

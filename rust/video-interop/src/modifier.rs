#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Modifier {
    Implicit,
    Explicit(u64),
}

impl Modifier {
    pub const fn linear() -> Self {
        Self::Explicit(0)
    }

    pub const fn explicit(self) -> Option<u64> {
        match self {
            Self::Implicit => None,
            Self::Explicit(value) => Some(value),
        }
    }
}

#[cfg(feature = "rustler")]
mod rustler_impl {
    use rustler::{Decoder, Encoder, Env, Error, NifResult, Term};

    use super::Modifier;

    mod atoms {
        rustler::atoms! {
            implicit
        }
    }

    impl<'a> Decoder<'a> for Modifier {
        fn decode(term: Term<'a>) -> NifResult<Self> {
            if let Ok(atom) = term.decode::<rustler::Atom>() {
                return if atom == atoms::implicit() {
                    Ok(Self::Implicit)
                } else {
                    Err(Error::BadArg)
                };
            }

            term.decode::<u64>().map(Self::Explicit)
        }
    }

    impl Encoder for Modifier {
        fn encode<'a>(&self, env: Env<'a>) -> Term<'a> {
            match self {
                Self::Implicit => atoms::implicit().encode(env),
                Self::Explicit(value) => value.encode(env),
            }
        }
    }
}

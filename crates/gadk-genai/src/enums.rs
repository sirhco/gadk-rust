// Copyright 2026 Chris Olson
// Portions derived from ADK-Go (https://github.com/google/adk-go), Copyright 2026 Google LLC.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

/// Defines a string enum: known variants plus `Unknown(String)`, serde as a plain string.
#[macro_export]
macro_rules! string_enum {
    ($(#[$m:meta])* $name:ident { $($(#[$vm:meta])* $variant:ident => $s:literal),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        pub enum $name {
            $($(#[$vm])* $variant,)+
            /// A value not known to this version of the crate; round-trips unchanged.
            Unknown(String),
        }
        impl $name {
            /// The wire string of this value.
            pub fn as_str(&self) -> &str {
                match self { $(Self::$variant => $s,)+ Self::Unknown(s) => s.as_str() }
            }
        }
        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result { f.write_str(self.as_str()) }
        }
        impl ::std::str::FromStr for $name {
            type Err = ::std::convert::Infallible;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Ok(match s { $($s => Self::$variant,)+ other => Self::Unknown(other.to_string()) })
            }
        }
        impl $crate::__private_serde::Serialize for $name {
            fn serialize<S: $crate::__private_serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> { s.serialize_str(self.as_str()) }
        }
        impl<'de> $crate::__private_serde::Deserialize<'de> for $name {
            fn deserialize<D: $crate::__private_serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let s = <String as $crate::__private_serde::Deserialize>::deserialize(d)?;
                Ok(s.parse().unwrap_or_else(|e: ::std::convert::Infallible| match e {}))
            }
        }
    };
}

//! Helper for stored records that keep unknown fields for round trips.
//!
//! `#[serde(flatten)]` deserialises by buffering the whole object into a
//! content tree before extracting fields. When a flattened field's type also
//! flattens, buffering nests and debug builds multiply the frame size until a
//! test thread overflows. The macro below generates a `Deserialize` that walks
//! the map once: known fields are read directly and everything else is
//! collected into `ExtraFields`. Serialisation still uses `#[serde(flatten)]`,
//! so the stored JSON is unchanged.

/// Generate a `Deserialize` that reads known fields from a map and collects the
/// unknown remainder into `ExtraFields` in a single pass.
///
/// `optional` fields default when absent (as serde does for `Option<T>`);
/// `required` fields fail when absent. Use this for any stored type that is
/// itself flattened into a parent, so only the parent buffers its input.
#[macro_export]
macro_rules! deserialize_with_extra {
    (
        $name:ident {
            extra: $extra:ident,
            required { $( $req:ident : $req_ty:ty ),* $(,)? }
            optional { $( $opt:ident : $opt_ty:ty ),* $(,)? }
        }
    ) => {
        impl<'de> ::serde::Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: ::serde::Deserializer<'de>,
            {
                struct RecordVisitor;

                impl<'de> ::serde::de::Visitor<'de> for RecordVisitor {
                    type Value = $name;

                    fn expecting(
                        &self,
                        formatter: &mut ::std::fmt::Formatter<'_>,
                    ) -> ::std::fmt::Result {
                        write!(formatter, "a {}", stringify!($name))
                    }

                    fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
                    where
                        M: ::serde::de::MapAccess<'de>,
                    {
                        $(
                            let mut $req: Option<$req_ty> = None;
                        )*
                        $(
                            let mut $opt: Option<$opt_ty> = None;
                        )*
                        let mut $extra = $crate::ExtraFields::new();
                        while let Some(key) = map.next_key::<String>()? {
                            match key.as_str() {
                                $(
                                    stringify!($req) => {
                                        $req = Some(map.next_value()?);
                                    }
                                )*
                                $(
                                    stringify!($opt) => {
                                        $opt = Some(map.next_value()?);
                                    }
                                )*
                                _ => {
                                    $extra.insert(
                                        key,
                                        map.next_value::<::serde_json::Value>()?,
                                    );
                                }
                            }
                        }
                        Ok($name {
                            $(
                                $req: $req.ok_or_else(|| {
                                    ::serde::de::Error::missing_field(stringify!($req))
                                })?,
                            )*
                            $(
                                $opt: $opt.unwrap_or_default(),
                            )*
                            $extra,
                        })
                    }
                }

                deserializer.deserialize_map(RecordVisitor)
            }
        }
    };
}

/*
 * Copyright 2018 The Starlark in Rust Authors.
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     https://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

//! Fixed set enumerations, with runtime checking of validity.
//!
//! Calling `enum()` produces an [`EnumType`]. Calling the [`EnumType`] creates an [`EnumValue`].
//!
//! The implementation ensures that each value of the enumeration is only stored once,
//! so they may also provide (modest) memory savings. Created in starlark with the
//! `enum` function:
//!
//! ```
//! # starlark::assert::pass(r#"
//! Colors = enum("Red", "Green", "Blue")
//! val = Colors("Red")
//! assert_eq(val.value, "Red")
//! assert_eq(val.index, 0)
//! assert_eq(Colors[0], val)
//! assert_eq(Colors.type, "Colors")
//! assert_eq([v.value for v in Colors], ["Red", "Green", "Blue"])
//! # "#);
//! ```
use crate as starlark;
use crate::{
    codemap::Span,
    collections::SmallMap,
    eval::{Evaluator, Parameters, ParametersParser, ParametersSpecBuilder},
    values::{
        function::{NativeFunction, FUNCTION_TYPE},
        index::convert_index,
        ComplexValue, Freezer, Heap, SimpleValue, StarlarkIterable, StarlarkValue, Trace, Value,
        ValueLike,
    },
};
use derivative::Derivative;
use gazebo::{any::AnyLifetime, cell::ARef};
use thiserror::Error;

#[derive(Error, Debug)]
enum EnumError {
    #[error("enum values must all be distinct, but repeated `{0}`")]
    DuplicateEnumValue(String),
    #[error("Unknown enum element `{0}`, given to `{1}`")]
    InvalidElement(String, String),
}

/// The type of an enumeration, created by `enum()`.
#[derive(Clone, Debug, Trace)]
// Deliberately store fully populated values
// for each entry, so we can produce enum values with zero allocation.
pub struct EnumTypeGen<V> {
    typ: Option<String>,
    // The key is the value of the enumeration
    // The value is a value of type EnumValue
    elements: SmallMap<V, V>,
    // Function to construct an enumeration, cached, so we don't recreate it on each invoke.
    constructor: V,
}

/// A value from an enumeration.
#[derive(Clone, Derivative, Trace)]
#[derivative(Debug)]
pub struct EnumValueGen<V> {
    // Must ignore value.typ or type.elements, since they are circular
    #[derivative(Debug = "ignore")]
    typ: V, // Must be EnumType it points back to (so it can get the type)
    value: V,   // The value of this enumeration
    index: i32, // The index in the enumeration
}

starlark_complex_value!(pub EnumType);
starlark_complex_value!(pub EnumValue);

impl<'v> ComplexValue<'v> for EnumType<'v> {
    // So we can get the name set and tie the cycle
    fn is_mutable(&self) -> bool {
        true
    }

    fn freeze(self: Box<Self>, freezer: &Freezer) -> anyhow::Result<Box<dyn SimpleValue>> {
        let mut elements = SmallMap::with_capacity(self.elements.len());
        for (k, t) in self.elements.into_iter_hashed() {
            elements.insert_hashed(k.freeze(freezer)?, t.freeze(freezer)?);
        }
        Ok(box FrozenEnumType {
            typ: self.typ,
            elements,
            constructor: self.constructor.freeze(freezer)?,
        })
    }

    fn export_as(&mut self, variable_name: &str, _eval: &mut Evaluator<'v, '_>) {
        if self.typ.is_none() {
            self.typ = Some(variable_name.to_owned())
        }
    }
}

impl<'v> ComplexValue<'v> for EnumValue<'v> {
    fn freeze(self: Box<Self>, freezer: &Freezer) -> anyhow::Result<Box<dyn SimpleValue>> {
        Ok(box FrozenEnumValue {
            typ: self.typ.freeze(freezer)?,
            value: self.value.freeze(freezer)?,
            index: self.index,
        })
    }
}

impl<'v> EnumType<'v> {
    pub(crate) fn new(elements: Vec<Value<'v>>, heap: &'v Heap) -> anyhow::Result<Value<'v>> {
        // We are constructing the enum and all elements in one go.
        // They both point at each other, which adds to the complexity.
        let typ = heap.alloc(EnumType {
            typ: None,
            elements: SmallMap::new(),
            constructor: heap.alloc(Self::make_constructor()),
        });

        let mut res = SmallMap::with_capacity(elements.len());
        for (i, x) in elements.iter().enumerate() {
            let v = heap.alloc(EnumValue {
                typ,
                index: i as i32,
                value: *x,
            });
            if res.insert_hashed(x.get_hashed()?, v).is_some() {
                return Err(EnumError::DuplicateEnumValue(x.to_string()).into());
            }
        }

        // Here we tie the cycle
        let mut t = EnumType::from_value_mut(typ)?.unwrap();
        t.elements = res;
        Ok(typ)
    }

    // The constructor is actually invariant in the enum type it works for, so we could try and allocate it
    // once for all enumerations. But that seems like a lot of work for not much benefit.
    fn make_constructor() -> NativeFunction {
        let mut signature = ParametersSpecBuilder::with_capacity("enum".to_owned(), 2);
        signature.required("$value");
        let signature = signature.build();

        // We want to get the value of `me` into the function, but that doesn't work since it
        // might move between therads - so we create the NativeFunction and apply it later.
        NativeFunction::new(
            move |eval, this, mut param_parser: ParametersParser| {
                let this = this.unwrap();
                let val: Value = param_parser.next("value", eval)?;
                let typ = EnumType::from_value(this).unwrap();
                match typ.elements.get_hashed(val.get_hashed()?.borrow()) {
                    Some(v) => Ok(*v),
                    None => {
                        Err(EnumError::InvalidElement(val.to_string(), this.to_string()).into())
                    }
                }
            },
            signature.signature(),
            signature,
        )
    }
}

impl<'v, V: ValueLike<'v>> EnumValueGen<V> {
    /// The result of calling `type()` on an enum value.
    pub const TYPE: &'static str = "enum";

    fn get_enum_type(&self) -> ARef<'v, EnumType<'v>> {
        // Safe to unwrap because we always ensure typ is EnumType
        EnumType::from_value(self.typ.to_value()).unwrap()
    }
}

impl<'v, V: ValueLike<'v>> StarlarkValue<'v> for EnumTypeGen<V>
where
    Self: AnyLifetime<'v>,
{
    starlark_type!(FUNCTION_TYPE);

    fn collect_repr(&self, collector: &mut String) {
        collector.push_str("enum(");
        for (i, (v, _)) in self.elements.iter().enumerate() {
            if i != 0 {
                collector.push_str(", ");
            }
            v.collect_repr(collector);
        }
        collector.push(')');
    }

    fn invoke(
        &self,
        me: Value<'v>,
        location: Option<Span>,
        mut params: Parameters<'v, '_>,
        eval: &mut Evaluator<'v, '_>,
    ) -> anyhow::Result<Value<'v>> {
        params.this = Some(me);
        self.constructor.invoke(location, params, eval)
    }

    fn length(&self) -> anyhow::Result<i32> {
        Ok(self.elements.len() as i32)
    }

    fn at(&self, index: Value, _heap: &'v Heap) -> anyhow::Result<Value<'v>> {
        let i = convert_index(index, self.elements.len() as i32)? as usize;
        // Must be in the valid range since convert_index checks that, so just unwrap
        Ok(self.elements.get_index(i).map(|x| *x.1).unwrap().to_value())
    }

    fn iterate(&self) -> anyhow::Result<&(dyn StarlarkIterable<'v> + 'v)> {
        Ok(self)
    }

    fn dir_attr(&self) -> Vec<String> {
        vec!["type".to_owned()]
    }

    fn has_attr(&self, attribute: &str) -> bool {
        attribute == "type"
    }

    fn get_attr(&self, attribute: &str, heap: &'v Heap) -> Option<Value<'v>> {
        if attribute == "type" {
            Some(heap.alloc(self.typ.as_deref().unwrap_or(EnumValue::TYPE)))
        } else {
            None
        }
    }

    fn equals(&self, other: Value<'v>) -> anyhow::Result<bool> {
        match EnumType::from_value(other) {
            Some(other) if self.typ == other.typ && self.elements.len() == other.elements.len() => {
                for (k1, k2) in self.elements.keys().zip(other.elements.keys()) {
                    if !k1.to_value().equals(*k2)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            _ => Ok(false),
        }
    }
}

impl<'v, V: ValueLike<'v>> StarlarkIterable<'v> for EnumTypeGen<V> {
    fn to_iter<'a>(&'a self, _heap: &'v Heap) -> Box<dyn Iterator<Item = Value<'v>> + 'a>
    where
        'v: 'a,
    {
        box self.elements.values().map(|x| x.to_value())
    }
}

impl<'v, V: ValueLike<'v>> StarlarkValue<'v> for EnumValueGen<V>
where
    Self: AnyLifetime<'v>,
{
    starlark_type!(EnumValue::TYPE);

    fn matches_type(&self, ty: &str) -> bool {
        ty == EnumValue::TYPE || Some(ty) == self.get_enum_type().typ.as_deref()
    }

    fn to_json(&self) -> anyhow::Result<String> {
        self.value.to_json()
    }

    fn collect_repr(&self, collector: &mut String) {
        self.value.collect_repr(collector)
    }

    fn equals(&self, other: Value<'v>) -> anyhow::Result<bool> {
        match EnumValue::from_value(other) {
            Some(other) if self.typ.equals(other.typ)? => Ok(self.index == other.index),
            _ => Ok(false),
        }
    }

    fn get_hash(&self) -> anyhow::Result<u64> {
        self.value.get_hash()
    }

    fn get_attr(&self, attribute: &str, _heap: &'v Heap) -> Option<Value<'v>> {
        match attribute {
            "index" => Some(Value::new_int(self.index)),
            "value" => Some(self.value.to_value()),
            _ => None,
        }
    }

    fn has_attr(&self, attribute: &str) -> bool {
        attribute == "index" || attribute == "value"
    }

    fn dir_attr(&self) -> Vec<String> {
        vec!["index".to_owned(), "value".to_owned()]
    }
}

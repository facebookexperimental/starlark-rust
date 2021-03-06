/*
 * Copyright 2019 The Starlark in Rust Authors.
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

// Possible optimisations:
// Avoid the Box duplication
// Encode Int in the pointer too

// We use pointer tagging on the bottom two bits:
// 00 => this Value pointer is actually a FrozenValue pointer
// 01 => this is a real Value pointer
// 11 => this is a bool (next bit: 1 => true, 0 => false)
// 10 => this is a None
//
// We don't use pointer tagging for Int (although we'd like to), because
// our val_ref requires a pointer to the value. We need to put that pointer
// somewhere. The solution is to have a separate value storage vs vtable.

use crate::values::{
    layout::{
        heap::{Freezer, Heap},
        pointer::{Pointer, PointerUnpack},
        pointer_i32::PointerI32,
    },
    none::NoneType,
    ComplexValue, ControlError, SimpleValue, StarlarkValue, Trace, Tracer,
};
use gazebo::{cell::ARef, prelude::*, variants::VariantName};
use static_assertions::assert_eq_size;
use std::{
    cell::{Cell, Ref, RefCell, RefMut},
    time::Instant,
};
use void::Void;

// So we can provide &dyn StarlarkValue's when we need them
const VALUE_NONE: NoneType = NoneType;
const VALUE_TRUE: bool = true;
const VALUE_FALSE: bool = false;

/// A Starlark value. The lifetime argument `'v` corresponds to the [`Heap`] it is stored on.
///
/// Many of the methods simply forward to the underlying [`StarlarkValue`].
#[derive(Clone_, Copy_, Dupe_)]
// One possible change: moving to Forward during GC.
// Will not be a `ValueMem::Ref` (see `ValueRef` for that).
pub struct Value<'v>(pub(crate) Pointer<'v, 'v, FrozenValueMem, ValueMem<'v>>);

/// A value that might have reference semantics.
/// References are required when a lambda captures an outer variable,
/// as all subsequent modifications of the outer variable will be
/// reflected in the inner.
/// However, most values captured are not by reference, so use the user_tag
/// to indicate whether a value is a `Ref` (and must be dereffed a lot),
/// or just a normal `Value` (much cheaper).
/// A normal `Value` cannot be `ValueMem::Ref`, but this one might be.
#[derive(Debug)]
pub(crate) struct ValueRef<'v>(pub(crate) Cell<Option<Value<'v>>>);

/// A [`Value`] that can never be changed. Can be converted back to a [`Value`] with [`to_value`](FrozenValue::to_value).
///
/// A [`FrozenValue`] exists on a [`FrozenHeap`](crate::values::FrozenHeap), which in turn can be kept
/// alive by a [`FrozenHeapRef`](crate::values::FrozenHeapRef). If the frozen heap gets dropped
/// while a [`FrozenValue`] from it still exists, the program will probably segfault, so be careful
/// when working directly with [`FrozenValue`]s. See the type [`OwnedFrozenValue`](crate::values::OwnedFrozenValue)
/// for a little bit more safety.
#[derive(Clone, Copy, Dupe)]
// One possible change: moving from Blackhole during GC
pub struct FrozenValue(pub(crate) Pointer<'static, 'static, FrozenValueMem, Void>);

// These can both be shared, but not obviously, because we hide a fake RefCell in Pointer to stop
// it having variance.
unsafe impl Send for FrozenValue {}
unsafe impl Sync for FrozenValue {}

// We care a lot about the size of these data types, so make sure they don't
// regress
assert_eq_size!(FrozenValueMem, [usize; 3]);
assert_eq_size!(ValueMem, [usize; 4]);

#[derive(VariantName)]
pub(crate) enum FrozenValueMem {
    #[allow(dead_code)] // That's the whole point of it
    Uninitialized(Void), // Never created (see Value::Uninitialized)
    Str(Box<str>),
    Blackhole, // Only occurs during a GC
    Simple(Box<dyn StarlarkValue<'static> + Send + Sync>),
}

fn simple_starlark_value<'a, 'v>(
    x: &'a (dyn StarlarkValue<'static> + Send + Sync),
) -> &'a dyn StarlarkValue<'v> {
    let x: &'a dyn StarlarkValue<'static> = x;
    unsafe { transmute!(&'a dyn StarlarkValue<'static>, &'a dyn StarlarkValue<'v>, x) }
}

#[derive(VariantName)]
pub(crate) enum ValueMem<'v> {
    // Never created, but we often get to ValueMem via dereferencing pointers
    // which have a moderate chance of pointing at 0's, so detect that special case
    // and give a workable error message
    #[allow(dead_code)] // That's the whole point of it
    Uninitialized(Void),
    // A literal string
    Str(Box<str>),
    // Occurs during freezing (for the to-space) - never encountered normally.
    Forward(FrozenValue),
    // Occurs during GC (for the to-space) - never encountered normally.
    Copied(Value<'v>),
    // Only occurs during GC
    Blackhole,
    // Things that aren't mutable and don't point to other Value's
    Simple(Box<dyn StarlarkValue<'static> + Send + Sync>),
    // Mutable things in my heap that aren't `is_mutable()`
    Immutable(Box<dyn ComplexValue<'v>>),
    // Mutable things that are in my heap and are `is_mutable()`
    Mutable(RefCell<Box<dyn ComplexValue<'v>>>),
    // Used references in slots - usually wrapped in ValueRef
    // Never points at a Ref, must point directly at a real value,
    // but might be unassigned (None)
    Ref(Cell<Option<Value<'v>>>),
    // Used for profiling
    CallEnter(Value<'v>, Instant),
    CallExit(Instant),
}

impl<'v> ValueMem<'v> {
    pub fn unexpected(&self, method: &str) -> ! {
        panic!(
            "ValueMem::{}, unexpected variant {}",
            method,
            self.variant_name()
        )
    }

    #[allow(clippy::borrowed_box)]
    fn unpack_box_str(&self) -> Option<&Box<str>> {
        match self {
            Self::Str(x) => Some(x),
            _ => None,
        }
    }

    fn unpack_str(&self) -> Option<&str> {
        match self {
            Self::Str(x) => Some(x),
            _ => None,
        }
    }

    fn get_ref_mut_opt(&self) -> Option<RefMut<dyn ComplexValue<'v>>> {
        match self {
            Self::Mutable(x) => match x.try_borrow_mut() {
                Err(_) => None,
                Ok(state) => Some(RefMut::map(state, |x| &mut **x)),
            },
            _ => None,
        }
    }

    fn get_ref_mut(&self) -> anyhow::Result<RefMut<dyn ComplexValue<'v>>> {
        match self {
            Self::Mutable(x) => match x.try_borrow_mut() {
                // Could be called by something else having the ref locked, but iteration is
                // definitely most likely
                Err(_) => Err(ControlError::MutationDuringIteration.into()),
                Ok(state) => Ok(RefMut::map(state, |x| &mut **x)),
            },
            _ => Err(ControlError::CannotMutateImmutableValue.into()),
        }
    }

    fn get_ref(&self) -> Option<&dyn StarlarkValue<'v>> {
        match self {
            Self::Str(x) => Some(x),
            Self::Simple(x) => Some(simple_starlark_value(Box::as_ref(x))),
            Self::Immutable(x) => Some(x.as_starlark_value()),
            Self::Mutable(_) => None,
            _ => self.unexpected("get_ref"),
        }
    }

    #[inline(always)] // There are only two callers
    pub(crate) fn get_aref(&'v self) -> ARef<'v, dyn StarlarkValue<'v>> {
        match self {
            Self::Str(x) => ARef::new_ptr(x),
            Self::Simple(x) => ARef::new_ptr(simple_starlark_value(Box::as_ref(x))),
            Self::Immutable(x) => ARef::new_ptr(x.as_starlark_value()),
            Self::Mutable(x) => ARef::new_ref(Ref::map(x.borrow(), |x| x.as_starlark_value())),
            _ => self.unexpected("get_aref"),
        }
    }
}

impl FrozenValueMem {
    fn unexpected(&self, method: &str) -> ! {
        panic!(
            "FrozenValueMem::{}, unexpected variant {}",
            method,
            self.variant_name()
        )
    }

    fn unpack_str(&self) -> Option<&str> {
        match self {
            Self::Str(x) => Some(x),
            _ => None,
        }
    }

    #[allow(clippy::borrowed_box)]
    fn unpack_box_str(&self) -> Option<&Box<str>> {
        match self {
            Self::Str(x) => Some(x),
            _ => None,
        }
    }

    fn get_ref<'v>(&self) -> &dyn StarlarkValue<'v> {
        match self {
            Self::Str(x) => x,
            Self::Simple(x) => simple_starlark_value(Box::as_ref(x)),
            _ => self.unexpected("get_ref"),
        }
    }
}

impl<'v> Value<'v> {
    /// Create a new `None` value.
    pub fn new_none() -> Self {
        Self(Pointer::new_none())
    }

    /// Create a new boolean.
    pub fn new_bool(x: bool) -> Self {
        Self(Pointer::new_bool(x))
    }

    /// Create a new integer.
    pub fn new_int(x: i32) -> Self {
        Self(Pointer::new_int(x))
    }

    /// Turn a [`FrozenValue`] into a [`Value`]. See the safety warnings on
    /// [`OwnedFrozenValue`](crate::values::OwnedFrozenValue).
    pub fn new_frozen(x: FrozenValue) -> Self {
        // Safe if every FrozenValue must have had a reference added to its heap first.
        // That property is NOT statically checked.
        let p = unsafe {
            transmute!(
                Pointer<'static, 'static, FrozenValueMem, Void>,
                Pointer<'v, 'static, FrozenValueMem, Void>,
                x.0
            )
        };
        Self(p.coerce())
    }

    /// Obtain the underlying [`FrozenValue`] from inside the [`Value`], if it is one.
    pub fn unpack_frozen(self) -> Option<FrozenValue> {
        unsafe {
            transmute!(
                Option<Pointer<'v, 'v, FrozenValueMem, Void>>,
                Option<Pointer<'static, 'static, FrozenValueMem, Void>>,
                self.0.coerce_opt()
            )
            .map(FrozenValue)
        }
    }

    /// Is this value `None`.
    pub fn is_none(self) -> bool {
        self.0.is_none()
    }

    /// Obtain the underlying `bool` if it is a boolean.
    pub fn unpack_bool(self) -> Option<bool> {
        self.0.unpack_bool()
    }

    /// Obtain the underlying `int` if it is an integer.
    pub fn unpack_int(self) -> Option<i32> {
        self.0.unpack_int()
    }

    /// Like [`unpack_str`](Value::unpack_str), but gives a pointer to a boxed string.
    /// Mostly useful for when you want to convert the string to a `dyn` trait, but can't
    /// form a `dyn` of an unsized type.
    ///
    /// Unstable and likely to be removed in future, as the presence of the `Box` is
    /// not a guaranteed part of the API.
    #[allow(clippy::borrowed_box)]
    pub fn unpack_box_str(self) -> Option<&'v Box<str>> {
        match self.0.unpack() {
            PointerUnpack::Ptr1(x) => x.unpack_box_str(),
            PointerUnpack::Ptr2(x) => x.unpack_box_str(),
            _ => None,
        }
    }

    /// Obtain the underlying `str` if it is a string.
    pub fn unpack_str(self) -> Option<&'v str> {
        match self.0.unpack() {
            PointerUnpack::Ptr1(x) => x.unpack_str(),
            PointerUnpack::Ptr2(x) => x.unpack_str(),
            _ => None,
        }
    }

    /// Get a pointer to a [`StarlarkValue`]. Will be [`None`] only when
    /// the underlying value is a [`ComplexValue`] which is marked
    /// [`is_mutable`](ComplexValue::is_mutable). If you want it to always
    /// produce a value, see [`get_aref`](Value::get_aref).
    pub fn get_ref(self) -> Option<&'v dyn StarlarkValue<'v>> {
        match self.0.unpack() {
            PointerUnpack::Ptr1(x) => Some(x.get_ref()),
            PointerUnpack::Ptr2(x) => x.get_ref(),
            PointerUnpack::None => Some(&VALUE_NONE),
            PointerUnpack::Bool(true) => Some(&VALUE_TRUE),
            PointerUnpack::Bool(false) => Some(&VALUE_FALSE),
            PointerUnpack::Int(x) => Some(PointerI32::new(x)),
        }
    }

    /// Get a pointer to a [`StarlarkValue`].
    pub fn get_aref(self) -> ARef<'v, dyn StarlarkValue<'v>> {
        match self.0.unpack() {
            PointerUnpack::Ptr1(x) => ARef::new_ptr(x.get_ref()),
            PointerUnpack::Ptr2(x) => x.get_aref(),
            PointerUnpack::None => ARef::new_ptr(&VALUE_NONE),
            PointerUnpack::Bool(x) => ARef::new_ptr(if x { &VALUE_TRUE } else { &VALUE_FALSE }),
            PointerUnpack::Int(x) => ARef::new_ptr(PointerI32::new(x)),
        }
    }

    // Like get_ref_mut, but only returns a mutable value if it's already mutable
    pub(crate) fn get_ref_mut_opt(self) -> Option<RefMut<'v, dyn ComplexValue<'v>>> {
        self.0.unpack_ptr2().and_then(|x| x.get_ref_mut_opt())
    }

    pub(crate) fn get_ref_mut(self) -> anyhow::Result<RefMut<'v, dyn ComplexValue<'v>>> {
        if let Some(x) = self.0.unpack_ptr2() {
            return x.get_ref_mut();
        }
        Err(ControlError::CannotMutateImmutableValue.into())
    }

    /// Are two [`Value`]s equal, looking at only their underlying pointer. This function is
    /// low-level and provides two guarantees.
    ///
    /// 1. It is _reflexive_, the same [`Value`] passed as both arguments will result in [`true`].
    /// 2. If this function is [`true`], then [`Value::equals`] will also consider them equal.
    ///
    /// Note that other properties are not guaranteed, and the result is not considered part of the API.
    /// The result can be impacted by optimisations such as hash-consing, copy-on-write, partial
    /// evaluation etc.
    pub fn ptr_eq(self, other: Self) -> bool {
        self.0.ptr_eq(other.0)
    }

    /// Get the underlying pointer.
    /// Should be done sparingly as it slightly breaks the abstraction.
    /// Most useful as a hash key based on pointer.
    pub(crate) fn ptr_value(self) -> usize {
        self.0.ptr_value()
    }
}

impl FrozenValue {
    /// Create a new value representing `None` in Starlark.
    pub fn new_none() -> Self {
        Self(Pointer::new_none())
    }

    /// Create a new boolean in Starlark.
    pub fn new_bool(x: bool) -> Self {
        Self(Pointer::new_bool(x))
    }

    /// Create a new int in Starlark.
    pub fn new_int(x: i32) -> Self {
        Self(Pointer::new_int(x))
    }

    /// Is a value a Starlark `None`.
    pub fn is_none(self) -> bool {
        self.0.is_none()
    }

    /// Return the [`bool`] if the value is a boolean, otherwise [`None`].
    pub fn unpack_bool(self) -> Option<bool> {
        self.0.unpack_bool()
    }

    /// Return the int if the value is an integer, otherwise [`None`].
    pub fn unpack_int(self) -> Option<i32> {
        self.0.unpack_int()
    }

    // The resulting `str` is alive as long as the `FrozenHeap` is,
    // but we don't have that lifetime available to us. Therefore,
    // we cheat a little, and use the lifetime of the `FrozenValue`.
    // Because of this cheating, we don't expose it outside Starlark.
    #[allow(clippy::trivially_copy_pass_by_ref)]
    pub(crate) fn unpack_str<'v>(&'v self) -> Option<&'v str> {
        match self.0.unpack_ptr1() {
            Some(x) => x.unpack_str(),
            _ => None,
        }
    }

    /// Get a pointer to the [`StarlarkValue`] object this value represents.
    pub fn get_ref<'v>(self) -> &'v dyn StarlarkValue<'v> {
        match self.0.unpack() {
            PointerUnpack::Ptr1(x) => x.get_ref(),
            PointerUnpack::Ptr2(x) => void::unreachable(*x),
            PointerUnpack::None => &VALUE_NONE,
            PointerUnpack::Bool(true) => &VALUE_TRUE,
            PointerUnpack::Bool(false) => &VALUE_FALSE,
            PointerUnpack::Int(x) => PointerI32::new(x),
        }
    }
}

unsafe impl<'v> Trace<'v> for ValueRef<'v> {
    fn trace(&mut self, tracer: &Tracer<'v>) {
        tracer.trace_ref(self)
    }
}

impl<'v> ValueRef<'v> {
    // Get the cell, chasing down any forwarding if it exists.
    // We have the invariant that if we have a ref we always set the user tag
    fn get_cell(&self) -> &Cell<Option<Value<'v>>> {
        match self.0.get() {
            Some(v) if v.0.get_user_tag() => match v.0.unpack_ptr2() {
                Some(ValueMem::Ref(cell)) => cell,
                _ => unreachable!(),
            },
            _ => &self.0,
        }
    }

    pub fn new_unassigned() -> Self {
        Self(Cell::new(None))
    }

    pub fn new_frozen(x: Option<FrozenValue>) -> Self {
        Self(Cell::new(x.map(Value::new_frozen)))
    }

    pub fn set(&self, value: Value<'v>) {
        self.get_cell().set(Some(value));
    }

    fn is_ref(&self) -> bool {
        match self.0.get() {
            Some(v) => v.0.get_user_tag(),
            _ => false,
        }
    }

    // Only valid if there is no chance this is a real ref
    pub fn set_direct(&self, value: Value<'v>) {
        debug_assert!(!self.is_ref());
        self.0.set(Some(value));
    }

    // Only valid if there is no chance this is a real ref
    pub fn get_direct(&self) -> Option<Value<'v>> {
        debug_assert!(!self.is_ref());
        self.0.get()
    }

    pub fn get(&self) -> Option<Value<'v>> {
        self.get_cell().get()
    }

    /// Return a new `ValueRef` that points at the same underlying memory as the original.
    /// Updates to either will result in both changing.
    pub fn clone_reference(&self, heap: &'v Heap) -> ValueRef<'v> {
        match self.0.get() {
            Some(v) if v.0.get_user_tag() => match v.0.unpack_ptr2() {
                Some(ValueMem::Ref(_)) => Self(Cell::new(Some(v))),
                _ => panic!(),
            },
            v => {
                let reffed = Value(heap.alloc_raw(ValueMem::Ref(Cell::new(v))).0.set_user_tag());
                self.0.set(Some(reffed));
                Self(Cell::new(Some(reffed)))
            }
        }
    }

    /// Create a duplicate `ValueRef` on something that must have been turned into a real ref,
    /// probably via `clone_reference`.
    pub fn dupe_reference(&self) -> ValueRef<'v> {
        debug_assert!(self.0.get().unwrap().0.get_user_tag());
        Self(self.0.dupe())
    }

    pub fn freeze(&self, freezer: &Freezer) -> anyhow::Result<Option<FrozenValue>> {
        self.get_cell().get().into_try_map(|x| freezer.freeze(x))
    }
}

/// A ['FrozenRef'] is essentially a ['FrozenValue'], and has the same memory and access guarantees
/// as it. However, this keeps the type of the type `T` of the actual ['FrozenValue'] as a owned
/// reference, allowing manipulation of the actual typed data.
#[derive(Clone_, Dupe_, Copy_, Debug)]
pub struct FrozenRef<T: 'static + ?Sized> {
    value: &'static T,
}

impl FrozenValue {
    pub fn downcast_frozen_ref<T: SimpleValue>(self) -> Option<FrozenRef<T>> {
        self.get_ref::<'static>()
            .as_dyn_any()
            .downcast_ref::<T>()
            .map(|t| FrozenRef { value: t })
    }
}

mod std_traits {
    use crate::values::layout::value::FrozenRef;
    use std::{
        borrow::Borrow,
        cmp::Ordering,
        hash::{Hash, Hasher},
        ops::Deref,
    };

    impl<T: ?Sized> Deref for FrozenRef<T> {
        type Target = T;

        fn deref(&self) -> &T {
            self.value
        }
    }

    impl<T: ?Sized> AsRef<T> for FrozenRef<T> {
        fn as_ref(&self) -> &T {
            &*self
        }
    }

    impl<T: ?Sized> Borrow<T> for FrozenRef<T> {
        fn borrow(&self) -> &T {
            &*self
        }
    }

    impl<T: ?Sized> PartialEq for FrozenRef<T>
    where
        T: PartialEq,
    {
        fn eq(&self, other: &Self) -> bool {
            (&*self as &T).eq(&*other as &T)
        }
    }

    impl<T: ?Sized> Eq for FrozenRef<T> where T: Eq {}

    impl<T: ?Sized> PartialOrd for FrozenRef<T>
    where
        T: PartialOrd,
    {
        fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
            (&*self as &T).partial_cmp(&*other as &T)
        }
    }

    impl<T: ?Sized> Ord for FrozenRef<T>
    where
        T: Ord,
    {
        fn cmp(&self, other: &Self) -> Ordering {
            (&*self as &T).cmp(&*other as &T)
        }
    }

    impl<T: ?Sized> Hash for FrozenRef<T>
    where
        T: Hash,
    {
        fn hash<H: Hasher>(&self, state: &mut H) {
            (&*self as &T).hash(state);
        }
    }
}

#[test]
fn test_send_sync()
where
    FrozenValue: Send + Sync,
{
}

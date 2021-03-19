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

//! A module with the standard function and constants that are by default in all
//! dialect of Starlark

use crate::{
    self as starlark,
    collections::SmallMap,
    environment::GlobalsBuilder,
    values::{
        bool::BOOL_VALUE_TYPE_NAME,
        dict::Dict,
        function::{NativeAttribute, WrappedMethod},
        int::INT_VALUE_TYPE_NAME,
        list::List,
        none::NoneType,
        range::Range,
        string::STRING_VALUE_TYPE_NAME,
        tuple::Tuple,
        Heap, Value,
    },
};
use anyhow::anyhow;
use gazebo::prelude::*;
use std::{cmp::Ordering, num::NonZeroI32};

fn unpack_pair<'v>(it: Value<'v>, heap: &'v Heap) -> anyhow::Result<(Value<'v>, Value<'v>)> {
    match it.iterate(heap) {
        Ok(it) => {
            let mut it = it.iter();
            let first = it.next();
            let second = it.next();
            let third = it.next();
            match (first, second, third) {
                (Some(k), Some(v), None) => return Ok((k, v)),
                _ => {}
            }
        }
        _ => {}
    }
    Err(anyhow!(
        "Found a non-pair element in the positional argument of dict(): {}",
        it.to_repr(),
    ))
}

#[starlark_module]
pub(crate) fn global_functions(builder: &mut GlobalsBuilder) {
    const None: NoneType = NoneType;
    const True: bool = true;
    const False: bool = false;

    /// fail: fail the execution
    ///
    /// Examples:
    /// ```
    /// # starlark::assert::fail(r#"
    /// fail("this is an error")  # error: this is an error
    /// # "#, "this is an error");
    /// ```
    fn fail(msg: Value) -> NoneType {
        Err(anyhow!("fail(): {}", msg))
    }

    /// [any](
    /// https://github.com/google/skylark/blob/a0e5de7e63b47e716cca7226662a4c95d47bf873/doc/spec.md#any
    /// ): returns true if any value in the iterable object have a truth value
    /// of true.
    ///
    /// Examples:
    ///
    /// ```
    /// # starlark::assert::all_true(r#"
    /// any([0, True]) == True
    /// any([0, 1]) == True
    /// any([0, 1, True]) == True
    /// any([0, 0]) == False
    /// any([0, False]) == False
    /// # "#);
    /// ```
    fn any(ref x: Value) -> bool {
        for i in &x.iterate(heap)? {
            if i.to_bool() {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// [all](
    /// https://github.com/google/skylark/blob/a0e5de7e63b47e716cca7226662a4c95d47bf873/doc/spec.md#all
    /// ): returns true if all values in the iterable object have a truth value
    /// of true.
    ///
    /// Examples:
    ///
    /// ```
    /// # starlark::assert::all_true(r#"
    /// all([1, True]) == True
    /// all([1, 1]) == True
    /// all([0, 1, True]) == False
    /// all([True, 1, True]) == True
    /// all([0, 0]) == False
    /// all([0, False]) == False
    /// all([True, 0]) == False
    /// all([1, False]) == False
    /// # "#);
    /// ```
    fn all(ref x: Value) -> bool {
        for i in &x.iterate(heap)? {
            if !i.to_bool() {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// [bool](
    /// https://github.com/google/skylark/blob/a0e5de7e63b47e716cca7226662a4c95d47bf873/doc/spec.md#bool
    /// ): returns the truth value of any starlark value.
    ///
    /// Examples:
    ///
    /// ```
    /// # starlark::assert::all_true(r#"
    /// bool() == False
    /// bool([]) == False
    /// bool([1]) == True
    /// bool(True) == True
    /// bool(False) == False
    /// bool(None) == False
    /// bool(bool) == True
    /// bool(1) == True
    /// bool(0) == False
    /// bool({}) == False
    /// bool({1:2}) == True
    /// bool(()) == False
    /// bool((1,)) == True
    /// bool("") == False
    /// bool("1") == True
    /// # "#);
    /// ```
    #[starlark_type(BOOL_VALUE_TYPE_NAME)]
    fn bool(ref x @ false: Value) -> bool {
        Ok(x.to_bool())
    }

    /// [chr](
    /// https://github.com/google/skylark/blob/a0e5de7e63b47e716cca7226662a4c95d47bf873/doc/spec.md#bool
    /// ): returns a string encoding a codepoint.
    ///
    /// `chr(i)` returns a returns a string that encodes the single Unicode code
    /// point whose value is specified by the integer `i`. `chr` fails
    /// unless `0 ≤ i ≤ 0x10FFFF`.
    ///
    /// Examples:
    ///
    /// ```
    /// # starlark::assert::all_true(r#"
    /// chr(65) == 'A'
    /// chr(1049) == 'Й'
    /// chr(0x1F63F) == '😿'
    /// # "#);
    /// ```
    fn chr(ref i: Value) -> String {
        let cp = i.to_int()? as u32;
        match std::char::from_u32(cp) {
            Some(x) => Ok(x.to_string()),
            None => Err(anyhow!(
                "chr() parameter value is 0x{:x} which is not a valid UTF-8 codepoint",
                cp
            )),
        }
    }

    /// [dict](
    /// https://github.com/google/skylark/blob/a0e5de7e63b47e716cca7226662a4c95d47bf873/doc/spec.md#dict
    /// ): creates a dictionary.
    ///
    /// `dict` creates a dictionary. It accepts up to one positional argument,
    /// which is interpreted as an iterable of two-element sequences
    /// (pairs), each specifying a key/value pair in the
    /// resulting dictionary.
    ///
    /// `dict` also accepts any number of keyword arguments, each of which
    /// specifies a key/value pair in the resulting dictionary; each keyword
    /// is treated as a string.
    ///
    /// Examples:
    ///
    /// ```
    /// # starlark::assert::all_true(r#"
    /// dict() == {}
    /// dict(**{'a': 1}) == {'a': 1}
    /// dict([(1, 2), (3, 4)]) == {1: 2, 3: 4}
    /// dict([(1, 2), ['a', 'b']]) == {1: 2, 'a': 'b'}
    /// dict(one=1, two=2) == {'one': 1, 'two': 2}
    /// dict([(1, 2)], x=3) == {1: 2, 'x': 3}
    /// dict([('x', 2)], x=3) == {'x': 3}
    /// # "#);
    /// # starlark::assert::is_true(r#"
    /// x = {'a': 1}
    /// y = dict([('x', 2)], **x)
    /// x == {'a': 1} and y == {'x': 2, 'a': 1}
    /// # "#);
    /// ```
    #[starlark_type(Dict::TYPE)]
    fn dict(ref a: Option<Value>, kwargs: Value) -> Value<'v> {
        match a {
            // Save to skip regenerating as we know that kwargs will always be a copy
            None => Ok(kwargs),
            Some(a) => {
                let mut result = SmallMap::new();
                match Dict::from_value(a) {
                    Some(mp) => {
                        for (k, v) in mp.iter_hashed() {
                            result.insert_hashed(k, v);
                        }
                    }
                    None => {
                        for el in &a.iterate(heap)? {
                            let (k, v) = unpack_pair(el, heap)?;
                            result.insert_hashed(k.get_hashed()?, v);
                        }
                    }
                }
                for (k, v) in Dict::from_value(kwargs).unwrap().iter_hashed() {
                    result.insert_hashed(k, v);
                }
                Ok(heap.alloc(Dict::new(result)))
            }
        }
    }

    /// [dir](
    /// https://github.com/google/skylark/blob/a0e5de7e63b47e716cca7226662a4c95d47bf873/doc/spec.md#dir
    /// ): list attributes of a value.
    ///
    /// `dir(x)` returns a list of the names of the attributes (fields and
    /// methods) of its operand. The attributes of a value `x` are the names
    /// `f` such that `x.f` is a valid expression.
    ///
    /// Examples:
    ///
    /// ```
    /// # starlark::assert::all_true(r#"
    /// "capitalize" in dir("abc")
    /// # "#);
    /// ```
    fn dir(ref x: Value) -> Vec<String> {
        let o = x.get_aref();
        let mut result;
        if let Some(members) = o.get_members() {
            result = members.names();
            result.extend(o.dir_attr())
        } else {
            result = o.dir_attr();
        };
        result.sort();
        Ok(result)
    }

    /// [enumerate](
    /// https://github.com/google/skylark/blob/a0e5de7e63b47e716cca7226662a4c95d47bf873/doc/spec.md#enumerate
    /// ): return a list of (index, element) from an iterable.
    ///
    /// `enumerate(x)` returns a list of `(index, value)` pairs, each containing
    /// successive values of the iterable sequence and the index of the
    /// value within the sequence.
    ///
    /// The optional second parameter, `start`, specifies an integer value to
    /// add to each index.
    ///
    /// Examples:
    ///
    /// ```
    /// # starlark::assert::all_true(r#"
    /// enumerate(["zero", "one", "two"]) == [(0, "zero"), (1, "one"), (2, "two")]
    /// enumerate(["one", "two"], 1) == [(1, "one"), (2, "two")]
    /// # "#);
    /// ```
    fn enumerate(ref it: Value, offset @ 0: i32) -> Vec<(i32, Value<'v>)> {
        let v = it
            .iterate(heap)?
            .iter()
            .enumerate()
            .map(|(k, v)| (k as i32 + offset, v))
            .collect();
        Ok(v)
    }

    /// [getattr](
    /// https://github.com/google/skylark/blob/a0e5de7e63b47e716cca7226662a4c95d47bf873/doc/spec.md#getattr
    /// ): returns the value of an attribute
    ///
    /// `getattr(x, name)` returns the value of the attribute (field or method)
    /// of x named `name`. It is a dynamic error if x has no such attribute.
    ///
    /// `getattr(x, "f")` is equivalent to `x.f`.
    ///
    /// ```
    /// # starlark::assert::all_true(r#"
    /// getattr("banana", "split")("a") == ["b", "n", "n", ""] # equivalent to "banana".split("a")
    /// # "#);
    /// ```
    fn getattr(ref a: Value, ref attr: &str, ref default: Option<Value>) -> Value<'v> {
        // Make sure we check get_type_value first, to be consistent with a.f
        match a.get_member(attr) {
            Some(v) => {
                if v.get_aref().is_function() {
                    // Insert self so the method see the object it is acting on
                    Ok(heap.alloc(WrappedMethod::new(a, v)))
                } else if let Some(v_attr) = v.downcast_ref::<NativeAttribute>() {
                    v_attr.call(a, ctx)
                } else {
                    Ok(v)
                }
            }
            None => match (default, a.get_attr(attr, heap)) {
                // A bit unfortunate we replace Err with a default value, potentially giving up
                // a valid error. But the error here is actually a well-formatted type-dependent
                // value that gives good information about why the lookup failed.
                (Some(default), Err(_)) => Ok(default),
                (_, res) => res,
            },
        }
    }

    /// [hasattr](
    /// https://github.com/google/skylark/blob/a0e5de7e63b47e716cca7226662a4c95d47bf873/doc/spec.md#hasattr
    /// ): test if an object has an attribute
    ///
    /// `hasattr(x, name)` reports whether x has an attribute (field or method)
    /// named `name`.
    fn hasattr(ref a: Value, ref attr: &str) -> bool {
        if a.get_member(attr).is_some() {
            Ok(true)
        } else {
            Ok(a.has_attr(attr))
        }
    }

    /// [hash](
    /// https://github.com/google/skylark/blob/a0e5de7e63b47e716cca7226662a4c95d47bf873/doc/spec.md#hash
    /// ): returns the hash number of a value.
    ///
    /// `hash(x)`` returns an integer hash value for x such that `x == y`
    /// implies `hash(x) == hash(y)``.
    ///
    /// `hash` fails if x, or any value upon which its hash depends, is
    /// unhashable.
    ///
    /// ```
    /// # starlark::assert::all_true(r#"
    /// hash("hello") != hash("world")
    /// # "#);
    /// ```
    fn hash(ref a: &str) -> i32 {
        // From the starlark spec:
        // > the hash function for strings is the same as that implemented by java.lang.String.hashCode,
        // > a simple polynomial accumulator over the UTF-16 transcoding of the string:
        // > `s[0]*31^(n-1) + s[1]*31^(n-2) + ... + s[n-1]`
        // As per spec the function should only support string and bytes types.
        // We don't have support for bytes, so parameter is forced to be a string.
        Ok(a.encode_utf16().fold(0i32, |hash: i32, c: u16| {
            31i32.wrapping_mul(hash).wrapping_add(c as i32)
        }))
    }

    /// [int](
    /// https://github.com/google/skylark/blob/a0e5de7e63b47e716cca7226662a4c95d47bf873/doc/spec.md#int
    /// ): convert a value to integer.
    ///
    /// `int(x[, base])` interprets its argument as an integer.
    ///
    /// If x is an `int`, the result is x.
    /// If x is a `float`, the result is the integer value nearest to x,
    /// truncating towards zero; it is an error if x is not finite (`NaN`,
    /// `+Inf`, `-Inf`).
    /// If x is a `bool`, the result is 0 for `False` or 1 for `True`.
    ///
    /// If x is a string, it is interpreted like a string literal;
    /// an optional base prefix (`0`, `0b`, `0B`, `0x`, `0X`) determines which
    /// base to use. The string may specify an arbitrarily large integer,
    /// whereas true integer literals are restricted to 64 bits.
    /// If a non-zero `base` argument is provided, the string is interpreted
    /// in that base and no base prefix is permitted; the base argument may
    /// specified by name.
    ///
    /// `int()` with no arguments returns 0.
    ///
    /// ```
    /// # starlark::assert::all_true(r#"
    /// int() == 0
    /// int(1) == 1
    /// int(False) == 0
    /// int(True) == 1
    /// int('1') == 1
    /// int('16') == 16
    /// int('16', 10) == 16
    /// int('16', 8) == 14
    /// int('16', 16) == 22
    /// # "#);
    /// # starlark::assert::fail(r#"
    /// int("hello")   # error: not a valid number
    /// # "#, "not a valid number");
    /// ```
    #[starlark_type(INT_VALUE_TYPE_NAME)]
    fn int(ref a: Option<Value>, base: Option<Value>) -> i32 {
        if a.is_none() {
            return Ok(0);
        }
        let a = a.unwrap();
        if let Some(s) = a.unpack_str() {
            let base = match base {
                Some(base) => base.to_int()?,
                None => 0,
            };
            if base == 1 || base < 0 || base > 36 {
                return Err(anyhow!(
                    "{} is not a valid base, int() base must be >= 2 and <= 36",
                    base
                ));
            }
            let (sign, s) = {
                match s.chars().next() {
                    Some('+') => (1, s.get(1..).unwrap()),
                    Some('-') => (-1, s.get(1..).unwrap()),
                    _ => (1, s),
                }
            };
            let base = if base == 0 {
                match s.get(0..2) {
                    Some("0b") | Some("0B") => 2,
                    Some("0o") | Some("0O") => 8,
                    Some("0x") | Some("0X") => 16,
                    _ => 10,
                }
            } else {
                base as u32
            };
            let s = match base {
                16 => {
                    if s.starts_with("0x") || s.starts_with("0X") {
                        s.get(2..).unwrap()
                    } else {
                        s
                    }
                }
                8 => {
                    if s.starts_with("0o") || s.starts_with("0O") {
                        s.get(2..).unwrap()
                    } else {
                        s
                    }
                }
                2 => {
                    if s.starts_with("0b") || s.starts_with("0B") {
                        s.get(2..).unwrap()
                    } else {
                        s
                    }
                }
                _ => s,
            };
            match i32::from_str_radix(s, base) {
                Ok(i) => Ok(sign * i),
                Err(x) => Err(anyhow!(
                    "{} is not a valid number in base {}: {}",
                    a.to_repr(),
                    base,
                    x,
                )),
            }
        } else {
            match base {
                Some(base) => Err(anyhow!(
                    "int() cannot convert non-string with explicit base '{}'",
                    base.to_repr()
                )),
                None => Ok(a.to_int()?),
            }
        }
    }

    /// [len](
    /// https://github.com/google/skylark/blob/a0e5de7e63b47e716cca7226662a4c95d47bf873/doc/spec.md#len
    /// ): get the length of a sequence
    ///
    /// `len(x)` returns the number of elements in its argument.
    ///
    /// It is a dynamic error if its argument is not a sequence.
    ///
    /// ```
    /// # starlark::assert::all_true(r#"
    /// len(()) == 0
    /// len({}) == 0
    /// len([]) == 0
    /// len([1]) == 1
    /// len([1,2]) == 2
    /// len({'16': 10}) == 1
    /// # "#);
    /// # starlark::assert::fail(r#"
    /// len(True)    # error: not supported
    /// # "#, "not supported");
    /// ```
    fn len(ref a: Value) -> i32 {
        Ok(a.length()?)
    }

    /// [list](
    /// https://github.com/google/skylark/blob/a0e5de7e63b47e716cca7226662a4c95d47bf873/doc/spec.md#list
    /// ): construct a list.
    ///
    /// `list(x)` returns a new list containing the elements of the
    /// iterable sequence x.
    ///
    /// With no argument, `list()` returns a new empty list.
    ///
    /// ```
    /// # starlark::assert::all_true(r#"
    /// list()        == []
    /// list((1,2,3)) == [1, 2, 3]
    /// # "#);
    /// # starlark::assert::fail(r#"
    /// list("strings are not iterable") # error: not supported
    /// # "#, "not supported");
    /// ```
    #[starlark_type(List::TYPE)]
    fn list(ref a: Option<Value>) -> Vec<Value<'v>> {
        let mut l = Vec::new();
        if let Some(a) = a {
            for x in &a.iterate(heap)? {
                l.push(x)
            }
        }
        Ok(l)
    }

    /// [max](
    /// https://github.com/google/skylark/blob/a0e5de7e63b47e716cca7226662a4c95d47bf873/doc/spec.md#max
    /// ): returns the maximum of a sequence.
    ///
    /// `max(x)` returns the greatest element in the iterable sequence x.
    ///
    /// It is an error if any element does not support ordered comparison,
    /// or if the sequence is empty.
    ///
    /// The optional named parameter `key` specifies a function to be applied
    /// to each element prior to comparison.
    ///
    /// ```
    /// # starlark::assert::all_true(r#"
    /// max([3, 1, 4, 1, 5, 9])               == 9
    /// max("two", "three", "four")           == "two"    # the lexicographically greatest
    /// max("two", "three", "four", key=len)  == "three"  # the longest
    /// # "#);
    /// ```
    fn max(mut args: Vec<Value>, key: Option<Value>) -> Value<'v> {
        let args = if args.len() == 1 {
            args.swap_remove(0)
        } else {
            heap.alloc(args)
        };
        let it = args.iterate(heap)?;
        let mut it = it.iter();
        let mut max = match it.next() {
            Some(x) => x,
            None => {
                return Err(anyhow!(
                    "Argument is an empty iterable, max() expect a non empty iterable"
                ));
            }
        };
        match key {
            None => {
                for i in it {
                    if max.compare(i)? == Ordering::Less {
                        max = i;
                    }
                }
            }
            Some(key) => {
                let mut invoker = key.new_invoker(heap)?;
                invoker.push_pos(max);
                let mut cached = invoker.invoke(key, None, ctx)?;
                for i in it {
                    let mut invoker = key.new_invoker(heap)?;
                    invoker.push_pos(i);
                    let keyi = invoker.invoke(key, None, ctx)?;
                    if cached.compare(keyi)? == Ordering::Less {
                        max = i;
                        cached = keyi;
                    }
                }
            }
        };
        Ok(max)
    }

    /// [min](
    /// https://github.com/google/skylark/blob/a0e5de7e63b47e716cca7226662a4c95d47bf873/doc/spec.md#min
    /// ): returns the minimum of a sequence.
    ///
    /// `min(x)` returns the least element in the iterable sequence x.
    ///
    /// It is an error if any element does not support ordered comparison,
    /// or if the sequence is empty.
    ///
    /// ```
    /// # starlark::assert::all_true(r#"
    /// min([3, 1, 4, 1, 5, 9])                 == 1
    /// min("two", "three", "four")             == "four"  # the lexicographically least
    /// min("two", "three", "four", key=len)    == "two"   # the shortest
    /// # "#);
    /// ```
    fn min(mut args: Vec<Value>, key: Option<Value>) -> Value<'v> {
        let args = if args.len() == 1 {
            args.swap_remove(0)
        } else {
            heap.alloc(args)
        };
        let it = args.iterate(heap)?;
        let mut it = it.iter();
        let mut min = match it.next() {
            Some(x) => x,
            None => {
                return Err(anyhow!(
                    "Argument is an empty iterable, min() expect a non empty iterable"
                ));
            }
        };
        match key {
            None => {
                for i in it {
                    if min.compare(i)? == Ordering::Greater {
                        min = i;
                    }
                }
            }
            Some(key) => {
                let mut invoker = key.new_invoker(heap)?;
                invoker.push_pos(min);
                let mut cached = invoker.invoke(key, None, ctx)?;
                for i in it {
                    let mut invoker = key.new_invoker(heap)?;
                    invoker.push_pos(i);
                    let keyi = invoker.invoke(key, None, ctx)?;
                    if cached.compare(keyi)? == Ordering::Greater {
                        min = i;
                        cached = keyi;
                    }
                }
            }
        };
        Ok(min)
    }

    /// [ord](
    /// https://github.com/google/skylark/blob/a0e5de7e63b47e716cca7226662a4c95d47bf873/doc/spec.mdord
    /// ): returns the codepoint of a character
    ///
    /// `ord(s)` returns the integer value of the sole Unicode code point
    /// encoded by the string `s`.
    ///
    /// If `s` does not encode exactly one Unicode code point, `ord` fails.
    /// Each invalid code within the string is treated as if it encodes the
    /// Unicode replacement character, U+FFFD.
    ///
    /// Example:
    ///
    /// ```
    /// # starlark::assert::all_true(r#"
    /// ord("A")                                == 65
    /// ord("Й")                                == 1049
    /// ord("😿")                               == 0x1F63F
    /// # "#);
    /// ```
    fn ord(ref a: Value) -> i32 {
        if let Some(s) = a.unpack_str() {
            let mut chars = s.chars();
            if let Some(c) = chars.next() {
                if chars.next().is_none() {
                    return Ok(u32::from(c) as i32);
                }
            }
        }
        Err(anyhow!(
            "ord(): {} is not a single character string",
            a.to_repr()
        ))
    }

    /// [range](
    /// https://github.com/google/skylark/blob/a0e5de7e63b47e716cca7226662a4c95d47bf873/doc/spec.md#range
    /// ): return a range of integers
    ///
    /// `range` returns a tuple of integers defined by the specified interval
    /// and stride.
    ///
    /// ```python
    /// range(stop)                             # equivalent to range(0, stop)
    /// range(start, stop)                      # equivalent to range(start, stop, 1)
    /// range(start, stop, step)
    /// ```
    ///
    /// `range` requires between one and three integer arguments.
    /// With one argument, `range(stop)` returns the ascending sequence of
    /// non-negative integers less than `stop`.
    /// With two arguments, `range(start, stop)` returns only integers not less
    /// than `start`.
    ///
    /// With three arguments, `range(start, stop, step)` returns integers
    /// formed by successively adding `step` to `start` until the value meets or
    /// passes `stop`. A call to `range` fails if the value of `step` is
    /// zero.
    ///
    /// ```
    /// # starlark::assert::all_true(r#"
    /// list(range(10))                         == [0, 1, 2, 3, 4, 5, 6, 7, 8, 9]
    /// list(range(3, 10))                      == [3, 4, 5, 6, 7, 8, 9]
    /// list(range(3, 10, 2))                   == [3, 5, 7, 9]
    /// list(range(10, 3, -2))                  == [10, 8, 6, 4]
    /// # "#);
    /// ```
    #[starlark_type(Range::TYPE)]
    fn range(ref a1: i32, ref a2: Option<i32>, ref step @ 1: i32) -> Range {
        let start = match a2 {
            None => 0,
            Some(_) => a1,
        };
        let stop = a2.unwrap_or(a1);
        let step = match NonZeroI32::new(step) {
            Some(step) => step,
            None => return Err(anyhow!("Third argument of range (step) cannot be zero")),
        };
        Ok(Range::new(start, stop, step))
    }

    /// [repr](
    /// https://github.com/google/skylark/blob/a0e5de7e63b47e716cca7226662a4c95d47bf873/doc/spec.md#repr
    /// ): formats its argument as a string.
    ///
    /// All strings in the result are double-quoted.
    ///
    /// ```
    /// # starlark::assert::all_true(r#"
    /// repr(1)                 == '1'
    /// repr("x")               == "\"x\""
    /// repr([1, "x"])          == "[1, \"x\"]"
    /// repr("test \"'")        == "\"test \\\"\\'\""
    /// repr("x\"y😿 \\'")      == "\"x\\\"y😿 \\\\\\'\""
    /// "#);
    /// ```
    fn repr(ref a: Value) -> String {
        Ok(a.to_repr())
    }

    /// [reversed](
    /// https://github.com/google/skylark/blob/a0e5de7e63b47e716cca7226662a4c95d47bf873/doc/spec.md#reversed
    /// ): reverse a sequence
    ///
    /// `reversed(x)` returns a new list containing the elements of the iterable
    /// sequence x in reverse order.
    ///
    /// ```
    /// # starlark::assert::all_true(r#"
    /// reversed(['a', 'b', 'c'])                       == ['c', 'b', 'a']
    /// reversed(range(5))                              == [4, 3, 2, 1, 0]
    /// reversed("stressed".split_codepoints())         == ["d", "e", "s", "s", "e", "r", "t", "s"]
    /// reversed({"one": 1, "two": 2}.keys())           == ["two", "one"]
    /// # "#);
    /// ```
    fn reversed(ref a: Value) -> Vec<Value<'v>> {
        let mut v: Vec<Value> = a.iterate(heap)?.iter().collect();
        v.reverse();
        Ok(v)
    }

    /// [sorted](
    /// https://github.com/google/skylark/blob/a0e5de7e63b47e716cca7226662a4c95d47bf873/doc/spec.md#sorted
    /// ): sort a sequence
    ///
    /// `sorted(x)` returns a new list containing the elements of the iterable
    /// sequence x, in sorted order.  The sort algorithm is stable.
    ///
    /// The optional named parameter `reverse`, if true, causes `sorted` to
    /// return results in reverse sorted order.
    ///
    /// The optional named parameter `key` specifies a function of one
    /// argument to apply to obtain the value's sort key.
    /// The default behavior is the identity function.
    ///
    /// ```
    /// # starlark::assert::all_true(r#"
    /// sorted([3, 1, 4, 1, 5, 9])                               == [1, 1, 3, 4, 5, 9]
    /// sorted([3, 1, 4, 1, 5, 9], reverse=True)                 == [9, 5, 4, 3, 1, 1]
    /// sorted(["two", "three", "four"], key=len)                == ["two", "four", "three"] # shortest to longest
    /// sorted(["two", "three", "four"], key=len, reverse=True)  == ["three", "four", "two"] # longest to shortest
    /// # "#);
    /// ```
    fn sorted(ref x: Value, key: Option<Value>, reverse @ false: Value) -> Vec<Value<'v>> {
        let it = x.iterate(heap)?;
        let x = it.iter();
        let mut it = match key {
            None => x.map(|x| (x, x)).collect(),
            Some(key) => {
                let mut v = Vec::new();
                for el in x {
                    let mut inv = key.new_invoker(heap)?;
                    inv.push_pos(el);
                    v.push((el, inv.invoke(key, None, ctx)?));
                }
                v
            }
        };

        let mut compare_ok = Ok(());

        let reverse = reverse.to_bool();
        it.sort_by(|x: &(Value, Value), y: &(Value, Value)| {
            let ord_or_err = if reverse {
                x.1.compare(y.1).map(Ordering::reverse)
            } else {
                x.1.compare(y.1)
            };
            match ord_or_err {
                Ok(r) => r,
                Err(e) => {
                    compare_ok = Err(e);
                    Ordering::Equal // does not matter
                }
            }
        });

        compare_ok?;

        let result: Vec<Value> = it.into_map(|x| x.0);
        Ok(result)
    }

    /// [str](
    /// https://github.com/google/skylark/blob/a0e5de7e63b47e716cca7226662a4c95d47bf873/doc/spec.md#str
    /// ): formats its argument as a string.
    ///
    /// If x is a string, the result is x (without quotation).
    /// All other strings, such as elements of a list of strings, are
    /// double-quoted.
    ///
    /// ```
    /// # starlark::assert::all_true(r#"
    /// str(1)                          == '1'
    /// str("x")                        == 'x'
    /// str([1, "x"])                   == "[1, \"x\"]"
    /// # "#);
    /// ```
    #[starlark_type(STRING_VALUE_TYPE_NAME)]
    fn str(ref a: Value) -> Value<'v> {
        if a.unpack_str().is_some() {
            // Special case that can avoid reallocating, but should be equivalent.
            // Relies on String.to_str giving back exactly what it started with (which it does)
            Ok(a)
        } else {
            Ok(heap.alloc(a.to_str()))
        }
    }

    /// [tuple](
    /// https://github.com/google/skylark/blob/a0e5de7e63b47e716cca7226662a4c95d47bf873/doc/spec.md#tuple
    /// ): returns a tuple containing the elements of the iterable x.
    ///
    /// With no arguments, `tuple()` returns the empty tuple.
    ///
    /// ```
    /// # starlark::assert::all_true(r#"
    /// tuple() == ()
    /// tuple([1,2,3]) == (1, 2, 3)
    /// # "#);
    #[starlark_type(Tuple::TYPE)]
    fn tuple(ref a: Option<Value>) -> Tuple<'v> {
        let mut l = Vec::new();
        if let Some(a) = a {
            for x in &a.iterate(heap)? {
                l.push(x)
            }
        }
        Ok(Tuple::new(l))
    }

    /// [type](
    /// https://github.com/google/skylark/blob/a0e5de7e63b47e716cca7226662a4c95d47bf873/doc/spec.md#type
    /// ): returns a string describing the type of its operand.
    ///
    /// ```
    /// # starlark::assert::all_true(r#"
    /// type(None)              == "NoneType"
    /// type(0)                 == "int"
    /// type(1)                 == "int"
    /// type(())                == "tuple"
    /// type("hello")           == "string"
    /// # "#);
    /// ```
    fn r#type(ref a: Value) -> Value<'v> {
        Ok(a.get_type_value().unpack().to_value())
    }

    /// [zip](
    /// https://github.com/google/skylark/blob/a0e5de7e63b47e716cca7226662a4c95d47bf873/doc/spec.md#zip
    /// ): zip several iterables together
    ///
    /// `zip()` returns a new list of n-tuples formed from corresponding
    /// elements of each of the n iterable sequences provided as arguments to
    /// `zip`.  That is, the first tuple contains the first element of each of
    /// the sequences, the second element contains the second element of each
    /// of the sequences, and so on.  The result list is only as long as the
    /// shortest of the input sequences.
    ///
    /// ```
    /// # starlark::assert::all_true(r#"
    /// zip()                                   == []
    /// zip(range(5))                           == [(0,), (1,), (2,), (3,), (4,)]
    /// zip(range(5), "abc".split_codepoints()) == [(0, "a"), (1, "b"), (2, "c")]
    /// # "#);
    /// ```
    fn zip(args: Vec<Value>) -> Vec<Value<'v>> {
        let mut v = Vec::new();
        let mut first = true;
        for arg in args {
            let mut idx = 0;
            for e in &arg.iterate(heap)? {
                if first {
                    v.push(heap.alloc((e,)));
                    idx += 1;
                } else if idx < v.len() {
                    v[idx] = v[idx].add(heap.alloc((e,)), heap)?;
                    idx += 1;
                }
            }
            v.truncate(idx);
            first = false;
        }
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use crate::assert;

    #[test]
    fn test_constants() {
        assert::is_true("not None");
        assert::is_true("not False");
        assert::is_true("True");
    }

    #[test]
    fn test_error_codes() {
        assert::fail("chr(0x110000)", "not a valid UTF-8");
    }

    #[test]
    fn test_hash() {
        assert::eq("0", "hash('')");
        assert::eq("97", "hash('a')");
        assert::eq("3105", "hash('ab')");
        assert::eq("96354", "hash('abc')");
        assert::eq("2987074", "hash('abcd')");
        assert::eq("92599395", "hash('abcde')");
        assert::eq("-1424385949", "hash('abcdef')");
        assert::all_true(
            r#"
hash("te") == hash("te")
hash("te") != hash("st")
x = "test"; y = "te" + "st"; hash(y) == hash(y)
"#,
        );
        assert::fail("hash(None)", "doesn't match");
        assert::fail("hash(True)", "doesn't match");
        assert::fail("hash(1)", "doesn't match");
        assert::fail("hash([])", "doesn't match");
        assert::fail("hash({})", "doesn't match");
        assert::fail("hash(range(1))", "doesn't match");
        assert::fail("hash((1, 2))", "doesn't match");
        assert::fail(
            r#"
def foo():
    pass
hash(foo)
"#,
            "doesn't match",
        );
    }
}

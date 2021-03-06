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

macro_rules! expr {
    ($name:expr, |$eval:ident| $body:expr) => {{
        #[allow(clippy::needless_question_mark)]
        let res: ExprCompiled = box move |$eval| $eval.ann($name, |$eval| Ok($body));
        ExprCompiledValue::Compiled(res)
    }};
    ($name:expr, $v1:ident, |$eval:ident| $body:expr) => {{
        let $v1 = $v1.as_compiled();
        #[allow(clippy::needless_question_mark)]
        let res: ExprCompiled = box move |$eval| {
            let $v1 = $v1($eval)?;
            $eval.ann($name, |$eval| Ok($body))
        };
        ExprCompiledValue::Compiled(res)
    }};
    ($name:expr, $v1:ident, $v2:ident, |$eval:ident| $body:expr) => {{
        let $v1 = $v1.as_compiled();
        let $v2 = $v2.as_compiled();
        #[allow(clippy::needless_question_mark)]
        let res: ExprCompiled = box move |$eval| {
            let $v1 = $v1($eval)?;
            let $v2 = $v2($eval)?;
            $eval.ann($name, |$eval| Ok($body))
        };
        ExprCompiledValue::Compiled(res)
    }};
}

macro_rules! value {
    ($v:expr) => {
        ExprCompiledValue::Value($v)
    };
}

macro_rules! stmt {
    ($name:expr, $span:ident, |$eval:ident| $body:expr) => {{
        box move |$eval| {
            $eval.ann($name, |$eval| {
                before_stmt($span, $eval);
                $body;
                #[allow(unreachable_code)]
                Ok(())
            })
        }
    }};
}

pub(crate) mod compr;
pub(crate) mod def;
pub(crate) mod expr;
pub(crate) mod stmt;

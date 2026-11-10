//! Ending an attempt with an error the script cannot catch.
//!
//! Boa 0.22 has no interrupt hook. A runtime-limit error is not catchable by `try`/`catch` and
//! skips `finally`, so the abort lowers the recursion limit to zero and calls an empty function
//! the engine itself created (the `Function` constructor with no arguments), then restores the
//! limits. The reason is kept in the run state; the error itself carries no meaning.
//!
//! Each abort on 0.22.0 leaks entry-frame stack slots in its context, so a context is never
//! reused after one: every attempt starts in a fresh context.

use boa_engine::{Context, JsError, JsNativeError, JsObject, JsResult, JsValue};

/// The engine-created empty function to call at recursion limit zero.
pub fn empty_function(ctx: &mut Context) -> JsResult<JsObject> {
    ctx.intrinsics().constructors().function().constructor().construct(&[], None, ctx)
}

/// An uncatchable error that unwinds the whole attempt.
pub fn uncatchable(ctx: &mut Context, empty: &JsObject) -> JsError {
    let saved = ctx.runtime_limits();
    ctx.runtime_limits_mut().set_recursion_limit(0);
    let result = empty.call(&JsValue::undefined(), &[], ctx);
    ctx.set_runtime_limits(saved);
    match result {
        Err(e) => e,
        // Not reached on 0.22.0; still unwind, as an ordinary error.
        Ok(_) => JsNativeError::error().with_message("script stopped").into(),
    }
}

//! Conversions between script values and the JSON used on the tape and by session commands.

use boa_engine::{Context, JsNativeError, JsResult, JsString, JsValue};
use serde_json::Value;

/// A script string as Rust text. A lone surrogate can't cross into the session, so it is a
/// `TypeError` the script can catch, at the call that passed it.
pub fn text(s: &JsString) -> JsResult<String> {
    s.to_std_string().map_err(|_| JsNativeError::typ().with_message("string contains an unpaired surrogate").into())
}

/// A script value as JSON for a host call. `undefined` becomes `null`.
pub fn to_json(v: &JsValue, ctx: &mut Context) -> JsResult<Value> {
    if let Some(s) = v.as_string() {
        return Ok(Value::String(text(&s)?));
    }
    Ok(v.to_json(ctx)?.unwrap_or(Value::Null))
}

pub fn from_json(v: &Value, ctx: &mut Context) -> JsResult<JsValue> {
    JsValue::from_json(v, ctx)
}

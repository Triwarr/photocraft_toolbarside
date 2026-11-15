//! The global objects scripts see. All of them are Rust natives; no JavaScript source is
//! evaluated besides the user's script.

use boa_engine::object::builtins::JsFunction;
use boa_engine::object::{FunctionObjectBuilder, ObjectInitializer};
use boa_engine::property::Attribute;
use boa_engine::{Context, Finalize, JsData, JsError, JsNativeError, JsObject, JsResult, JsString, JsValue, NativeFunction, Trace, js_string};
use serde_json::{Value, json};

use crate::runner::{Shared, Stop};
use crate::tape::{Begin, Call, Outcome};
use crate::{APP_VERSION, ModalKind, abort, value};

/// The engine-created empty function used by [`abort::uncatchable`].
pub(crate) struct Abort(pub JsObject);

type Native = fn(&JsValue, &[JsValue], &mut Context) -> JsResult<JsValue>;

/// The document a `Document` object stands for, by tab index.
#[derive(Debug, Clone, Trace, Finalize, JsData)]
#[boa_gc(unsafe_empty_trace)]
struct DocRef(usize);

fn shared(ctx: &Context) -> JsResult<Shared> {
    ctx.get_data::<Shared>().cloned().ok_or_else(|| JsNativeError::error().with_message("script host is missing").into())
}

fn error(message: impl Into<String>) -> JsError {
    JsNativeError::error().with_message(message.into()).into()
}

/// End the attempt with an uncatchable error, remembering why.
fn stop(ctx: &mut Context, why: Stop) -> JsError {
    if let Ok(s) = shared(ctx) {
        s.borrow_mut().stop = Some(why);
    }
    match ctx.get_data::<Abort>().map(|a| a.0.clone()) {
        Some(empty) => abort::uncatchable(ctx, &empty),
        None => error("script host is missing"),
    }
}

/// Start a taped call: charge the budget, then answer from the tape or report it live.
fn begin(ctx: &mut Context, call: Call) -> JsResult<Result<Outcome, usize>> {
    let s = shared(ctx)?;
    let begun = {
        let mut st = s.borrow_mut();
        // A stop the script swallowed (only possible if the abort ever became catchable) must not
        // reach the host: every later call stops again.
        if st.stop.is_some() || !st.budget.charge() { None } else { Some(st.tape.begin(call)) }
    };
    match begun {
        None => {
            let why = s.borrow_mut().stop.take().unwrap_or(Stop::TimeLimit);
            Err(stop(ctx, why))
        }
        Some(Err(divergence)) => Err(stop(ctx, Stop::Fault(divergence))),
        Some(Ok(Begin::Replay(o))) => Ok(Ok(o)),
        Some(Ok(Begin::Live(at))) => Ok(Err(at)),
    }
}

/// Run a session command through the tape.
pub(crate) fn command(ctx: &mut Context, id: &str, params: Value) -> JsResult<Value> {
    let outcome = match begin(ctx, Call::Command { id: id.into(), params: params.clone() })? {
        Ok(o) => o,
        Err(at) => {
            let s = shared(ctx)?;
            let mut st = s.borrow_mut();
            let o = match st.host.execute(id, params) {
                Ok(v) => Outcome::Return(v),
                Err(e) => Outcome::Throw(e.0),
            };
            st.tape.finish(at, o.clone());
            o
        }
    };
    match outcome {
        Outcome::Return(v) => Ok(v),
        Outcome::Throw(m) => Err(error(m)),
        Outcome::Modal(_) => Err(stop(ctx, Stop::Fault("replay divergence: a command was recorded as a dialog".into()))),
    }
}

fn modal(ctx: &mut Context, kind: ModalKind, args: &[JsValue]) -> JsResult<JsValue> {
    let args = args.iter().map(|a| value::to_json(a, ctx)).collect::<JsResult<Vec<_>>>()?;
    match begin(ctx, Call::Modal { kind, args: Value::Array(args) })? {
        Ok(Outcome::Modal(Some(answer))) => match (kind, answer) {
            (ModalKind::Alert, _) => Ok(JsValue::undefined()),
            (ModalKind::Confirm, v) => Ok(JsValue::from(v.as_bool().unwrap_or(false))),
            (ModalKind::Prompt, Value::String(s)) => Ok(JsValue::from(JsString::from(s.as_str()))),
            (ModalKind::Prompt, _) => Ok(JsValue::null()),
        },
        // Unanswered (live now, or still open from an earlier attempt): pause the run.
        Ok(Outcome::Modal(None)) | Err(_) => Err(stop(ctx, Stop::Paused)),
        Ok(_) => Err(stop(ctx, Stop::Fault("replay divergence: a dialog was recorded as a command".into()))),
    }
}

fn alert(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    modal(ctx, ModalKind::Alert, args)
}

fn confirm(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    modal(ctx, ModalKind::Confirm, args)
}

fn prompt(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    modal(ctx, ModalKind::Prompt, args)
}

fn writeln(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let mut line = String::new();
    for a in args {
        line.push_str(&value::text(&a.to_string(ctx)?)?);
    }
    if let Err(at) = begin(ctx, Call::Output(line.clone()))? {
        let s = shared(ctx)?;
        let mut st = s.borrow_mut();
        st.host.write_line(&line);
        st.tape.finish(at, Outcome::Return(Value::Null));
    }
    Ok(JsValue::undefined())
}

fn random(_: &JsValue, _: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let s = shared(ctx)?;
    let mut st = s.borrow_mut();
    let v = match st.tape.take_random() {
        Some(v) => v,
        None => {
            let v = st.next_random();
            st.tape.push_random(v);
            v
        }
    };
    Ok(JsValue::from(v))
}

fn app_name(_: &JsValue, _: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let name = shared(ctx)?.borrow().app_name.clone();
    Ok(JsValue::from(JsString::from(name.as_str())))
}

fn session_inspect(ctx: &mut Context) -> JsResult<Value> {
    command(ctx, "session.inspect", json!({}))
}

fn document_object(index: usize, ctx: &mut Context) -> JsObject {
    let getters: [(JsString, Native); 5] = [
        (js_string!("name"), doc_name),
        (js_string!("width"), doc_width),
        (js_string!("height"), doc_height),
        (js_string!("resolution"), doc_resolution),
        (js_string!("saved"), doc_saved),
    ];
    let getters: Vec<(JsString, JsFunction)> = getters.into_iter().map(|(k, f)| (k, getter(ctx, f))).collect();
    let mut init = ObjectInitializer::with_native_data(DocRef(index), ctx);
    for (k, g) in getters {
        init.accessor(k, Some(g), None, Attribute::ENUMERABLE | Attribute::CONFIGURABLE);
    }
    init.build()
}

fn getter(ctx: &mut Context, f: Native) -> JsFunction {
    FunctionObjectBuilder::new(ctx.realm(), NativeFunction::from_fn_ptr(f)).length(0).build()
}

fn this_doc(this: &JsValue, ctx: &mut Context) -> JsResult<Value> {
    let index = this.as_object().and_then(|o| o.downcast_ref::<DocRef>().map(|d| d.0)).ok_or_else(|| JsNativeError::typ().with_message("not a Document"))?;
    command(ctx, "document.inspect", json!({ "document": index }))
}

fn field(doc: &Value, key: &str, ctx: &mut Context) -> JsResult<JsValue> {
    value::from_json(doc.get(key).unwrap_or(&Value::Null), ctx)
}

fn doc_name(this: &JsValue, _: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let d = this_doc(this, ctx)?;
    field(&d, "name", ctx)
}

fn doc_width(this: &JsValue, _: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let d = this_doc(this, ctx)?;
    field(&d, "width", ctx)
}

fn doc_height(this: &JsValue, _: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let d = this_doc(this, ctx)?;
    field(&d, "height", ctx)
}

fn doc_resolution(this: &JsValue, _: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let d = this_doc(this, ctx)?;
    field(&d, "resolution", ctx)
}

fn doc_saved(this: &JsValue, _: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let index = this.as_object().and_then(|o| o.downcast_ref::<DocRef>().map(|d| d.0));
    let s = session_inspect(ctx)?;
    let dirty = s["documents"].as_array().and_then(|docs| index.and_then(|i| docs.get(i))).map(|d| d["dirty"].as_bool().unwrap_or(false));
    Ok(JsValue::from(!dirty.ok_or_else(|| error("the document is no longer open"))?))
}

fn app_documents(_: &JsValue, _: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let s = session_inspect(ctx)?;
    let count = s["documents"].as_array().map_or(0, Vec::len);
    let docs: Vec<JsValue> = (0..count).map(|i| document_object(i, ctx).into()).collect();
    Ok(boa_engine::object::builtins::JsArray::from_iter(docs, ctx).into())
}

fn app_active_document(_: &JsValue, _: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let s = session_inspect(ctx)?;
    let active = s["active"].as_u64().and_then(|i| usize::try_from(i).ok()).ok_or_else(|| error("No document is open."))?;
    Ok(document_object(active, ctx).into())
}

fn app_set_active_document(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let index = args
        .first()
        .and_then(JsValue::as_object)
        .and_then(|o| o.downcast_ref::<DocRef>().map(|d| d.0))
        .ok_or_else(|| JsNativeError::typ().with_message("activeDocument must be set to a Document"))?;
    command(ctx, "document.activate", json!({ "document": index }))?;
    Ok(JsValue::undefined())
}

/// Install the globals into a fresh context.
pub(crate) fn install(ctx: &mut Context) -> JsResult<()> {
    for (name, f, len) in [("alert", alert as Native, 1), ("confirm", confirm, 1), ("prompt", prompt, 2)] {
        ctx.register_global_builtin_callable(JsString::from(name), len, NativeFunction::from_fn_ptr(f))?;
    }

    let get_name = getter(ctx, app_name);
    let get_documents = getter(ctx, app_documents);
    let get_active = getter(ctx, app_active_document);
    let set_active = getter(ctx, app_set_active_document);
    let app = ObjectInitializer::new(ctx)
        .accessor(js_string!("name"), Some(get_name), None, Attribute::ENUMERABLE)
        .property(js_string!("version"), js_string!(APP_VERSION), Attribute::ENUMERABLE)
        .accessor(js_string!("documents"), Some(get_documents), None, Attribute::ENUMERABLE)
        .accessor(js_string!("activeDocument"), Some(get_active), Some(set_active), Attribute::ENUMERABLE)
        .build();
    ctx.register_global_property(js_string!("app"), app, Attribute::all())?;

    let dollar = ObjectInitializer::new(ctx).function(NativeFunction::from_fn_ptr(writeln), js_string!("writeln"), 1).build();
    ctx.register_global_property(js_string!("$"), dollar, Attribute::all())?;

    let math = ctx.global_object().get(js_string!("Math"), ctx)?;
    if let Some(math) = math.as_object() {
        let f = FunctionObjectBuilder::new(ctx.realm(), NativeFunction::from_fn_ptr(random)).name(js_string!("random")).length(0).build();
        math.set(js_string!("random"), f, true, ctx)?;
    }
    Ok(())
}

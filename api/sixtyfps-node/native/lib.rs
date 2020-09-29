/* LICENSE BEGIN
    This file is part of the SixtyFPS Project -- https://sixtyfps.io
    Copyright (c) 2020 Olivier Goffart <olivier.goffart@sixtyfps.io>
    Copyright (c) 2020 Simon Hausmann <simon.hausmann@sixtyfps.io>

    SPDX-License-Identifier: GPL-3.0-only
    This file is also available under commercial licensing terms.
    Please contact info@sixtyfps.io for more information.
LICENSE END */
use core::cell::RefCell;
use neon::prelude::*;
use sixtyfps_compilerlib::typeregister::Type;
use sixtyfps_corelib::Resource;

use std::rc::Rc;

mod persistent_context;

struct WrappedComponentType(Option<Rc<sixtyfps_interpreter::ComponentDescription>>);
struct WrappedComponentBox(Option<Rc<sixtyfps_interpreter::ComponentBox>>);

/// We need to do some gymnastic with closures to pass the ExecuteContext with the right lifetime
type GlobalContextCallback =
    dyn for<'b> Fn(&mut ExecuteContext<'b>, &persistent_context::PersistentContext<'b>);
scoped_tls_hkt::scoped_thread_local!(static GLOBAL_CONTEXT:
    for <'a> &'a dyn Fn(&GlobalContextCallback));

/// This function exists as a workaround so one can access the ExecuteContext from signal handler
fn run_scoped<'cx, T>(
    cx: &mut impl Context<'cx>,
    object_with_persistant_context: Handle<'cx, JsObject>,
    functor: impl FnOnce() -> Result<T, String>,
) -> NeonResult<T> {
    let persistent_context =
        persistent_context::PersistentContext::from_object(cx, object_with_persistant_context)?;
    Ok(cx
        .execute_scoped(|cx| {
            let cx = RefCell::new(cx);
            let cx_fn = move |callback: &GlobalContextCallback| {
                callback(&mut *cx.borrow_mut(), &persistent_context)
            };
            GLOBAL_CONTEXT.set(&&cx_fn, functor)
        })
        .or_else(|e| cx.throw_error(e))?)
}

/// Load a .60 files.
///
/// The first argument of this finction is a string to the .60 file
///
/// The return value is a SixtyFpsComponentType
fn load(mut cx: FunctionContext) -> JsResult<JsValue> {
    let path = cx.argument::<JsString>(0)?.value();
    let path = std::path::Path::new(path.as_str());
    let include_paths = match std::env::var_os("SIXTYFPS_INCLUDE_PATH") {
        Some(paths) => {
            std::env::split_paths(&paths).filter(|path| !path.as_os_str().is_empty()).collect()
        }
        None => vec![],
    };
    let compiler_config = sixtyfps_compilerlib::CompilerConfiguration {
        include_paths: &include_paths,
        ..Default::default()
    };
    let source = std::fs::read_to_string(&path).or_else(|e| cx.throw_error(e.to_string()))?;
    let (c, warnings) = match sixtyfps_interpreter::load(source, &path, &compiler_config) {
        (Ok(c), warnings) => (c, warnings),
        (Err(()), errors) => {
            errors.print();
            return cx.throw_error("Compilation error");
        }
    };

    warnings.print();

    let mut obj = SixtyFpsComponentType::new::<_, JsValue, _>(&mut cx, std::iter::empty())?;
    cx.borrow_mut(&mut obj, |mut obj| obj.0 = Some(c));
    Ok(obj.as_value(&mut cx))
}

fn create<'cx>(
    cx: &mut CallContext<'cx, impl neon::object::This>,
    component_type: Rc<sixtyfps_interpreter::ComponentDescription>,
) -> JsResult<'cx, JsValue> {
    let component = component_type.clone().create();
    let persistent_context = persistent_context::PersistentContext::new(cx);

    if let Some(args) = cx.argument_opt(0).and_then(|arg| arg.downcast::<JsObject>().ok()) {
        let properties = component_type.properties();
        for x in args.get_own_property_names(cx)?.to_vec(cx)? {
            let prop_name = x.to_string(cx)?.value();
            let value = args.get(cx, x)?;
            let ty = properties
                .get(&prop_name)
                .ok_or(())
                .or_else(|()| {
                    cx.throw_error(format!("Property {} not found in the component", prop_name))
                })?
                .clone();
            if let Type::Signal { .. } = ty {
                let _fun = value.downcast_or_throw::<JsFunction, _>(cx)?;
                let fun_idx = persistent_context.allocate(cx, value);
                component_type
                    .set_signal_handler(
                        component.borrow(),
                        prop_name.as_str(),
                        Box::new(move |args| {
                            let args = args.iter().cloned().collect::<Vec<_>>();
                            GLOBAL_CONTEXT.with(move |cx_fn| {
                                cx_fn(&move |cx, presistent_context| {
                                    let args = args
                                        .iter()
                                        .map(|a| to_js_value(a.clone(), cx).unwrap())
                                        .collect::<Vec<_>>();
                                    presistent_context
                                        .get(cx, fun_idx)
                                        .unwrap()
                                        .downcast::<JsFunction>()
                                        .unwrap()
                                        .call::<_, _, JsValue, _>(cx, JsUndefined::new(), args)
                                        .unwrap();
                                })
                            })
                        }),
                    )
                    .or_else(|_| cx.throw_error(format!("Cannot set signal")))?;
            } else {
                let value = to_eval_value(value, ty, cx)?;
                component_type
                    .set_property(component.borrow(), prop_name.as_str(), value)
                    .or_else(|_| cx.throw_error(format!("Cannot assign property")))?;
            }
        }
    }

    let mut obj = SixtyFpsComponent::new::<_, JsValue, _>(cx, std::iter::empty())?;
    persistent_context.save_to_object(cx, obj.downcast().unwrap());
    cx.borrow_mut(&mut obj, |mut obj| obj.0 = Some(Rc::new(component)));
    Ok(obj.as_value(cx))
}

fn to_eval_value<'cx>(
    val: Handle<JsValue>,
    ty: sixtyfps_compilerlib::typeregister::Type,
    cx: &mut impl Context<'cx>,
) -> NeonResult<sixtyfps_interpreter::Value> {
    use sixtyfps_interpreter::Value;
    match ty {
        Type::Float32 | Type::Int32 | Type::Duration | Type::Length | Type::LogicalLength => {
            Ok(Value::Number(val.downcast_or_throw::<JsNumber, _>(cx)?.value()))
        }
        Type::String => Ok(Value::String(val.to_string(cx)?.value().into())),
        Type::Color | Type::Array(_) | Type::Object(_) => todo!(),
        Type::Resource => Ok(Value::String(val.to_string(cx)?.value().into())),
        Type::Bool => Ok(Value::Bool(val.downcast_or_throw::<JsBoolean, _>(cx)?.value())),
        Type::Component(c) if c.root_element.borrow().base_type == Type::Void => {
            let obj = val.downcast_or_throw::<JsObject, _>(cx)?;
            Ok(Value::Object(
                c.root_element
                    .borrow()
                    .property_declarations
                    .iter()
                    .map(|(pro_name, pro_decl)| {
                        Ok((
                            pro_name.clone(),
                            to_eval_value(
                                obj.get(cx, pro_name.as_str())?,
                                pro_decl.property_type.clone(),
                                cx,
                            )?,
                        ))
                    })
                    .collect::<Result<_, _>>()?,
            ))
        }
        Type::Enumeration(_) => todo!(),
        Type::EnumerationValue(_) => todo!(),
        Type::Invalid
        | Type::Void
        | Type::Builtin(_)
        | Type::Native(_)
        | Type::Function { .. }
        | Type::Model
        | Type::Signal { .. }
        | Type::Easing
        | Type::Component(_) // The struct case is handled before
        | Type::PathElements 
        | Type::ElementReference => cx.throw_error("Cannot convert to a Sixtyfps property value"),
    }
}

fn to_js_value<'cx>(
    val: sixtyfps_interpreter::Value,
    cx: &mut impl Context<'cx>,
) -> NeonResult<Handle<'cx, JsValue>> {
    use sixtyfps_interpreter::Value;
    Ok(match val {
        Value::Void => JsUndefined::new().as_value(cx),
        Value::Number(n) => JsNumber::new(cx, n).as_value(cx),
        Value::String(s) => JsString::new(cx, s.as_str()).as_value(cx),
        Value::Bool(b) => JsBoolean::new(cx, b).as_value(cx),
        Value::Resource(r) => match r {
            Resource::None => JsUndefined::new().as_value(cx),
            Resource::AbsoluteFilePath(path) => JsString::new(cx, path.as_str()).as_value(cx),
            Resource::EmbeddedData { .. } | Resource::EmbeddedRgbaImage { .. } => {
                JsNull::new().as_value(cx)
            } // TODO: maybe pass around node buffers?
        },
        Value::Array(a) => {
            let js_array = JsArray::new(cx, a.len() as _);
            for (i, e) in a.into_iter().enumerate() {
                let v = to_js_value(e, cx)?;
                js_array.set(cx, i as u32, v)?;
            }
            js_array.as_value(cx)
        }
        Value::Object(o) => {
            let js_object = JsObject::new(cx);
            for (k, e) in o.into_iter() {
                let v = to_js_value(e, cx)?;
                js_object.set(cx, k.as_str(), v)?;
            }
            js_object.as_value(cx)
        }
        Value::Color(c) => JsNumber::new(cx, c.as_argb_encoded()).as_value(cx),
        Value::PathElements(_) => todo!(),
        Value::EasingCurve(_) => todo!(),
        Value::EnumerationValue(..) => todo!(),
        Value::ElementReference(..) => todo!(),
    })
}

declare_types! {
    class SixtyFpsComponentType for WrappedComponentType {
        init(_) {
            Ok(WrappedComponentType(None))
        }
        method create(mut cx) {
            let this = cx.this();
            let ct = cx.borrow(&this, |x| x.0.clone());
            let ct = ct.ok_or(()).or_else(|()| cx.throw_error("Invalid type"))?;
            create(&mut cx, ct)
        }
        method name(mut cx) {
            let this = cx.this();
            let ct = cx.borrow(&this, |x| x.0.clone());
            let ct = ct.ok_or(()).or_else(|()| cx.throw_error("Invalid type"))?;
            Ok(cx.string(ct.id()).as_value(&mut cx))
        }
        method properties(mut cx) {
            let this = cx.this();
            let ct = cx.borrow(&this, |x| x.0.clone());
            let ct = ct.ok_or(()).or_else(|()| cx.throw_error("Invalid type"))?;
            let properties = ct.properties();
            let array = JsArray::new(&mut cx, properties.len() as u32);
            let mut len: u32 = 0;
            for (p, _) in properties.iter().filter(|(_, prop_type)| prop_type.is_property_type()) {
                let prop_name = JsString::new(&mut cx, p);
                array.set(&mut cx, len, prop_name)?;
                len = len + 1;
            }
            Ok(array.as_value(&mut cx))
        }
        method signals(mut cx) {
            let this = cx.this();
            let ct = cx.borrow(&this, |x| x.0.clone());
            let ct = ct.ok_or(()).or_else(|()| cx.throw_error("Invalid type"))?;
            let properties = ct.properties();
            let array = JsArray::new(&mut cx, properties.len() as u32);
            let mut len: u32 = 0;
            for (p, _) in properties.iter().filter(|(_, prop_type)| matches!(**prop_type, Type::Signal{..})) {
                let prop_name = JsString::new(&mut cx, p);
                array.set(&mut cx, len, prop_name)?;
                len = len + 1;
            }
            Ok(array.as_value(&mut cx))
        }
    }

    class SixtyFpsComponent for WrappedComponentBox {
        init(_) {
            Ok(WrappedComponentBox(None))
        }
        method show(mut cx) {
            let mut this = cx.this();
            let component = cx.borrow(&mut this, |x| x.0.clone());
            let component = component.ok_or(()).or_else(|()| cx.throw_error("Invalid type"))?;
            run_scoped(&mut cx,this.downcast().unwrap(), || {
                component.window().run(component.borrow(), component.root_item());
                Ok(())
            })?;
            Ok(JsUndefined::new().as_value(&mut cx))
        }
        method get_property(mut cx) {
            let prop_name = cx.argument::<JsString>(0)?.value();
            let this = cx.this();
            let lock = cx.lock();
            let x = this.borrow(&lock).0.clone();
            let component = x.ok_or(()).or_else(|()| cx.throw_error("Invalid type"))?;
            let value = component.description()
                .get_property(component.borrow(), prop_name.as_str())
                .or_else(|_| cx.throw_error(format!("Cannot read property")))?;
            to_js_value(value, &mut cx)
        }
        method set_property(mut cx) {
            let prop_name = cx.argument::<JsString>(0)?.value();
            let this = cx.this();
            let lock = cx.lock();
            let x = this.borrow(&lock).0.clone();
            let component  = x.ok_or(()).or_else(|()| cx.throw_error("Invalid type"))?;
            let ty = component.description().properties()
                .get(&prop_name)
                .ok_or(())
                .or_else(|()| {
                    cx.throw_error(format!("Property {} not found in the component", prop_name))
                })?
                .clone();

            let value = to_eval_value(cx.argument::<JsValue>(1)?, ty, &mut cx)?;
            component.description()
                .set_property(component.borrow(), prop_name.as_str(), value)
                .or_else(|_| cx.throw_error(format!("Cannot assign property")))?;

            Ok(JsUndefined::new().as_value(&mut cx))
        }
        method emit_signal(mut cx) {
            let signal_name = cx.argument::<JsString>(0)?.value();
            let arguments = cx.argument::<JsArray>(1)?.to_vec(&mut cx)?;
            let this = cx.this();
            let lock = cx.lock();
            let x = this.borrow(&lock).0.clone();
            let component = x.ok_or(()).or_else(|()| cx.throw_error("Invalid type"))?;
            let ty = component.description().properties().get(&signal_name)
                .ok_or(())
                .or_else(|()| {
                    cx.throw_error(format!("Signal {} not found in the component", signal_name))
                })?
                .clone();
            let args = if let Type::Signal {args} = ty {
                let count = args.len();
                let args = arguments.into_iter().zip(args.into_iter()).map(|(a, ty)| to_eval_value(a, ty, &mut cx)).collect::<Result<Vec<_>, _>>()?;
                if args.len() != count {
                    cx.throw_error(format!("{} expect {} arguments, but {} where provided", signal_name, count, args.len()))?;
                }
                args

            } else {
                cx.throw_error(format!("{} is not a signal", signal_name))?;
                unreachable!()
            };

            run_scoped(&mut cx,this.downcast().unwrap(), || {
                component.description()
                    .emit_signal(component.borrow(), signal_name.as_str(), args.as_slice())
                    .map_err(|()| "Cannot emit signal".to_string())
            })?;
            Ok(JsUndefined::new().as_value(&mut cx))
        }

        method send_mouse_click(mut cx) {
            let x = cx.argument::<JsNumber>(0)?.value() as f32;
            let y = cx.argument::<JsNumber>(1)?.value() as f32;
            let this = cx.this();
            let lock = cx.lock();
            let comp = this.borrow(&lock).0.clone();
            let component = comp.ok_or(()).or_else(|()| cx.throw_error("Invalid type"))?;
            run_scoped(&mut cx,this.downcast().unwrap(), || {
                sixtyfps_corelib::tests::sixtyfps_send_mouse_click(component.borrow(), x, y, &component.window());
                Ok(())
            })?;
            Ok(JsUndefined::new().as_value(&mut cx))
        }

        method send_keyboard_string_sequence(mut cx) {
            let sequence = cx.argument::<JsString>(0)?.value();
            let this = cx.this();
            let lock = cx.lock();
            let comp = this.borrow(&lock).0.clone();
            let component = comp.ok_or(()).or_else(|()| cx.throw_error("Invalid type"))?;
            run_scoped(&mut cx,this.downcast().unwrap(), || {
                sixtyfps_corelib::tests::send_keyboard_string_sequence(component.borrow(), &sequence.into(), &component.window());
                Ok(())
            })?;
            Ok(JsUndefined::new().as_value(&mut cx))
        }
    }
}

register_module!(mut m, {
    m.export_function("load", load)?;
    m.export_function("mock_elapsed_time", mock_elapsed_time)?;
    Ok(())
});

/// let some time ellapse for testing purposes
fn mock_elapsed_time(mut cx: FunctionContext) -> JsResult<JsValue> {
    let ms = cx.argument::<JsNumber>(0)?.value();
    sixtyfps_corelib::tests::sixtyfps_mock_elapsed_time(ms as _);
    Ok(JsUndefined::new().as_value(&mut cx))
}

/* LICENSE BEGIN
    This file is part of the SixtyFPS Project -- https://sixtyfps.io
    Copyright (c) 2020 Olivier Goffart <olivier.goffart@sixtyfps.io>
    Copyright (c) 2020 Simon Hausmann <simon.hausmann@sixtyfps.io>

    SPDX-License-Identifier: GPL-3.0-only
    This file is also available under commercial licensing terms.
    Please contact info@sixtyfps.io for more information.
LICENSE END */
use crate::dynamic_component::InstanceRef;
use core::convert::{TryFrom, TryInto};
use core::iter::FromIterator;
use core::pin::Pin;
use sixtyfps_compilerlib::expression_tree::{
    BuiltinFunction, EasingCurve, Expression, ExpressionSpanned, NamedReference, Path as ExprPath,
    PathElement as ExprPathElement,
};
use sixtyfps_compilerlib::{object_tree::ElementRc, typeregister::Type};
use sixtyfps_corelib as corelib;
use sixtyfps_corelib::{
    graphics::PathElement, items::ItemRef, items::PropertyAnimation, Color, PathData, Resource,
    SharedArray, SharedString, Signal,
};
use std::collections::HashMap;
use std::rc::Rc;

pub trait ErasedPropertyInfo {
    fn get(&self, item: Pin<ItemRef>) -> Value;
    fn set(&self, item: Pin<ItemRef>, value: Value, animation: Option<PropertyAnimation>);
    fn set_binding(
        &self,
        item: Pin<ItemRef>,
        binding: Box<dyn Fn() -> Value>,
        animation: Option<PropertyAnimation>,
    );
    fn offset(&self) -> usize;

    unsafe fn link_two_ways(&self, item: Pin<ItemRef>, property2: *const ());
}

impl<Item: vtable::HasStaticVTable<corelib::items::ItemVTable>> ErasedPropertyInfo
    for &'static dyn corelib::rtti::PropertyInfo<Item, Value>
{
    fn get(&self, item: Pin<ItemRef>) -> Value {
        (*self).get(ItemRef::downcast_pin(item).unwrap()).unwrap()
    }
    fn set(&self, item: Pin<ItemRef>, value: Value, animation: Option<PropertyAnimation>) {
        (*self).set(ItemRef::downcast_pin(item).unwrap(), value, animation).unwrap()
    }
    fn set_binding(
        &self,
        item: Pin<ItemRef>,
        binding: Box<dyn Fn() -> Value>,
        animation: Option<PropertyAnimation>,
    ) {
        (*self).set_binding(ItemRef::downcast_pin(item).unwrap(), binding, animation).unwrap();
    }
    fn offset(&self) -> usize {
        (*self).offset()
    }
    unsafe fn link_two_ways(&self, item: Pin<ItemRef>, property2: *const ()) {
        (*self).link_two_ways(ItemRef::downcast_pin(item).unwrap(), property2)
    }
}

#[derive(Debug, Clone, PartialEq)]
/// This is a dynamically typed Value used in the interpreter, it need to be able
/// to be converted from and to anything that can be stored in a Property
pub enum Value {
    /// There is nothing in this value. That's the default.
    /// For example, a function that do not return a result would return a Value::Void
    Void,
    /// An i32 or a float
    Number(f64),
    /// String
    String(SharedString),
    /// Bool
    Bool(bool),
    /// A resource (typically an image)
    Resource(Resource),
    /// An Array
    Array(Vec<Value>),
    /// An object
    Object(HashMap<String, Value>),
    /// A color
    Color(Color),
    /// The elements of a path
    PathElements(PathData),
    /// An easing curve
    EasingCurve(corelib::animations::EasingCurve),
    /// An enumation, like TextHorizontalAlignment::align_center
    EnumerationValue(String, String),
    /// A reference to an element
    ElementReference(ElementRc),
}

impl Default for Value {
    fn default() -> Self {
        Value::Void
    }
}

impl corelib::rtti::ValueType for Value {}

/// Helper macro to implement the TryFrom / TryInto for Value
///
/// For example
/// `declare_value_conversion!(Number => [u32, u64, i32, i64, f32, f64] );`
/// means that Value::Number can be converted to / from each of the said rust types
macro_rules! declare_value_conversion {
    ( $value:ident => [$($ty:ty),*] ) => {
        $(
            impl TryFrom<$ty> for Value {
                type Error = ();
                fn try_from(v: $ty) -> Result<Self, ()> {
                    //Ok(Value::$value(v.try_into().map_err(|_|())?))
                    Ok(Value::$value(v as _))
                }
            }
            impl TryInto<$ty> for Value {
                type Error = ();
                fn try_into(self) -> Result<$ty, ()> {
                    match self {
                        //Self::$value(x) => x.try_into().map_err(|_|()),
                        Self::$value(x) => Ok(x as _),
                        _ => Err(())
                    }
                }
            }
        )*
    };
}
declare_value_conversion!(Number => [u32, u64, i32, i64, f32, f64, usize, isize] );
declare_value_conversion!(String => [SharedString] );
declare_value_conversion!(Bool => [bool] );
declare_value_conversion!(Resource => [Resource] );
declare_value_conversion!(Object => [HashMap<String, Value>] );
declare_value_conversion!(Color => [Color] );
declare_value_conversion!(PathElements => [PathData]);
declare_value_conversion!(EasingCurve => [corelib::animations::EasingCurve]);

macro_rules! declare_value_enum_conversion {
    ($ty:ty, $n:ident) => {
        impl TryFrom<$ty> for Value {
            type Error = ();
            fn try_from(v: $ty) -> Result<Self, ()> {
                Ok(Value::EnumerationValue(stringify!($n).to_owned(), v.to_string()))
            }
        }
        impl TryInto<$ty> for Value {
            type Error = ();
            fn try_into(self) -> Result<$ty, ()> {
                use std::str::FromStr;
                match self {
                    Self::EnumerationValue(enumeration, value) => {
                        if enumeration != stringify!($n) {
                            return Err(());
                        }

                        <$ty>::from_str(value.as_str()).map_err(|_| ())
                    }
                    _ => Err(()),
                }
            }
        }
    };
}

declare_value_enum_conversion!(corelib::items::TextHorizontalAlignment, TextHorizontalAlignment);
declare_value_enum_conversion!(corelib::items::TextVerticalAlignment, TextVerticalAlignment);

/// The local variable needed for binding evaluation
#[derive(Default)]
pub struct EvalLocalContext {
    local_variables: HashMap<String, Value>,
    function_arguments: Vec<Value>,
}

impl EvalLocalContext {
    /// Create a context for a function and passing the arguments
    pub fn from_function_arguments(function_arguments: Vec<Value>) -> Self {
        Self { function_arguments, ..Default::default() }
    }
}

/// Evaluate an expression and return a Value as the result of this expression
pub fn eval_expression(
    e: &Expression,
    component: InstanceRef,
    local_context: &mut EvalLocalContext,
) -> Value {
    match e {
        Expression::Invalid => panic!("invalid expression while evaluating"),
        Expression::Uncompiled(_) => panic!("uncompiled expression while evaluating"),
        Expression::TwoWayBinding(_) => panic!("invalid expression while evaluating"),
        Expression::StringLiteral(s) => Value::String(s.into()),
        Expression::NumberLiteral(n, unit) => Value::Number(unit.normalize(*n)),
        Expression::BoolLiteral(b) => Value::Bool(*b),
        Expression::SignalReference { .. } => panic!("signal in expression"),
        Expression::BuiltinFunctionReference(_) => panic!(
            "naked builtin function reference not allowed, should be handled by function call"
        ),
        Expression::ElementReference(_) => todo!("Element references are only supported in the context of built-in function calls at the moment"),
        Expression::PropertyReference(NamedReference { element, name }) => {
            load_property(component, &element.upgrade().unwrap(), name.as_ref()).unwrap()
        }
        Expression::RepeaterIndexReference { element } => load_property(
            component,
            &element.upgrade().unwrap().borrow().base_type.as_component().root_element,
            "index",
        )
        .unwrap(),
        Expression::RepeaterModelReference { element } => load_property(
            component,
            &element.upgrade().unwrap().borrow().base_type.as_component().root_element,
            "model_data",
        )
        .unwrap(),
        Expression::FunctionParameterReference { index, .. } => {
            local_context.function_arguments[*index].clone()
        }
        Expression::ObjectAccess { base, name } => {
            if let Value::Object(mut o) = eval_expression(base, component, local_context) {
                o.remove(name).unwrap_or(Value::Void)
            } else {
                Value::Void
            }
        }
        Expression::Cast { from, to } => {
            let v = eval_expression(&*from, component, local_context);
            match (v, to) {
                (Value::Number(n), Type::Int32) => Value::Number(n.round()),
                (Value::Number(n), Type::String) => {
                    Value::String(SharedString::from(format!("{}", n).as_str()))
                }
                (Value::Number(n), Type::Color) => Value::Color(Color::from_argb_encoded(n as u32)),
                (v, _) => v,
            }
        }
        Expression::CodeBlock(sub) => {
            let mut v = Value::Void;
            for e in sub {
                v = eval_expression(e, component, local_context);
            }
            v
        }
        Expression::FunctionCall { function, arguments } => match &**function {
            Expression::SignalReference(NamedReference { element, name }) => {
                let a = arguments.iter().map(|e| eval_expression(e, component, local_context));
                let element = element.upgrade().unwrap();
                generativity::make_guard!(guard);
                let enclosing_component =
                    enclosing_component_for_element(&element, component, guard);
                let component_type = enclosing_component.component_type;

                let item_info = &component_type.items[element.borrow().id.as_str()];
                let item = unsafe { item_info.item_from_component(enclosing_component.as_ptr()) };

                if let Some(signal_offset) = item_info.rtti.signals.get(name.as_str()) {
                    let signal =
                        unsafe { &*(item.as_ptr().add(*signal_offset) as *const Signal<()>) };
                    signal.emit(&());
                } else if let Some(signal_offset) = component_type.custom_signals.get(name.as_str())
                {
                    let signal = signal_offset.apply(&*enclosing_component.instance);
                    signal.emit(a.collect::<Vec<_>>().as_slice())
                } else {
                    panic!("unkown signal {}", name)
                }

                Value::Void
            }
            Expression::BuiltinFunctionReference(BuiltinFunction::GetWindowScaleFactor) => {
                Value::Number(window_ref(component).unwrap().scale_factor() as _)
            }
            Expression::BuiltinFunctionReference(BuiltinFunction::Debug) => {
                let a = arguments.iter().map(|e| eval_expression(e, component, local_context));
                println!("{:?}", a);
                Value::Void
            }
            Expression::BuiltinFunctionReference(BuiltinFunction::SetFocusItem) => {
                if arguments.len() != 1 {
                    panic!("internal error: incorrect argument count to SetFocusItem")
                }
                if let Expression::ElementReference(focus_item) = &arguments[0] {
                    generativity::make_guard!(guard);
                    let component_ref: Pin<vtable::VRef<corelib::component::ComponentVTable>> = unsafe {
                        Pin::new_unchecked(vtable::VRef::from_raw(
                            core::ptr::NonNull::from(&component.component_type.ct).cast(),
                            core::ptr::NonNull::from(&*component.as_ptr()),
                        ))
                    };

                    let enclosing_component =
                        enclosing_component_for_element(&focus_item, component, guard);
                    let component_type = enclosing_component.component_type;

                    let item_info = &component_type.items[focus_item.borrow().id.as_str()];
                    let item =
                        unsafe { item_info.item_from_component(enclosing_component.as_ptr()) };

                    window_ref(component).unwrap().set_focus_item(component_ref, item);
                    Value::Void
                } else {
                    panic!("internal error: argument to SetFocusItem must be an element")
                }
            }
            _ => panic!("call of something not a signal"),
        },
        Expression::SelfAssignment { lhs, rhs, op } => match &**lhs {
            Expression::PropertyReference(NamedReference { element, name }) => {
                let rhs = eval_expression(&**rhs, component, local_context);
                if *op == '=' {
                    store_property(component, &element.upgrade().unwrap(), name.as_ref(), rhs)
                        .unwrap();
                    return Value::Void;
                }
                let eval = |lhs| match (lhs, rhs, op) {
                    (Value::Number(a), Value::Number(b), '+') => Value::Number(a + b),
                    (Value::Number(a), Value::Number(b), '-') => Value::Number(a - b),
                    (Value::Number(a), Value::Number(b), '/') => Value::Number(a / b),
                    (Value::Number(a), Value::Number(b), '*') => Value::Number(a * b),
                    (lhs, rhs, op) => panic!("unsupported {:?} {} {:?}", lhs, op, rhs),
                };
                let element = element.upgrade().unwrap();
                generativity::make_guard!(guard);
                let enclosing_component =
                    enclosing_component_for_element(&element, component, guard);

                let component = element.borrow().enclosing_component.upgrade().unwrap();
                if element.borrow().id == component.root_element.borrow().id {
                    if let Some(x) = enclosing_component.component_type.custom_properties.get(name)
                    {
                        unsafe {
                            let p =
                                Pin::new_unchecked(&*enclosing_component.as_ptr().add(x.offset));
                            x.prop.set(p, eval(x.prop.get(p).unwrap()), None).unwrap();
                        }
                        return Value::Void;
                    }
                };
                let item_info =
                    &enclosing_component.component_type.items[element.borrow().id.as_str()];
                let item = unsafe { item_info.item_from_component(enclosing_component.as_ptr()) };
                let p = &item_info.rtti.properties[name.as_str()];
                p.set(item, eval(p.get(item)), None);
                Value::Void
            }
            _ => panic!("typechecking should make sure this was a PropertyReference"),
        },
        Expression::BinaryExpression { lhs, rhs, op } => {
            let lhs = eval_expression(&**lhs, component, local_context);
            let rhs = eval_expression(&**rhs, component, local_context);

            match (op, lhs, rhs) {
                ('+', Value::Number(a), Value::Number(b)) => Value::Number(a + b),
                ('-', Value::Number(a), Value::Number(b)) => Value::Number(a - b),
                ('/', Value::Number(a), Value::Number(b)) => Value::Number(a / b),
                ('*', Value::Number(a), Value::Number(b)) => Value::Number(a * b),
                ('<', Value::Number(a), Value::Number(b)) => Value::Bool(a < b),
                ('>', Value::Number(a), Value::Number(b)) => Value::Bool(a > b),
                ('≤', Value::Number(a), Value::Number(b)) => Value::Bool(a <= b),
                ('≥', Value::Number(a), Value::Number(b)) => Value::Bool(a >= b),
                ('=', a, b) => Value::Bool(a == b),
                ('!', a, b) => Value::Bool(a != b),
                ('&', Value::Bool(a), Value::Bool(b)) => Value::Bool(a && b),
                ('|', Value::Bool(a), Value::Bool(b)) => Value::Bool(a || b),
                (op, lhs, rhs) => panic!("unsupported {:?} {} {:?}", lhs, op, rhs),
            }
        }
        Expression::UnaryOp { sub, op } => {
            let sub = eval_expression(&**sub, component, local_context);
            match (sub, op) {
                (Value::Number(a), '+') => Value::Number(a),
                (Value::Number(a), '-') => Value::Number(-a),
                (Value::Bool(a), '!') => Value::Bool(!a),
                (sub, op) => panic!("unsupported {} {:?}", op, sub),
            }
        }
        Expression::ResourceReference { absolute_source_path } => {
            Value::Resource(Resource::AbsoluteFilePath(absolute_source_path.into()))
        }
        Expression::Condition { condition, true_expr, false_expr } => {
            match eval_expression(&**condition, component, local_context).try_into()
                as Result<bool, _>
            {
                Ok(true) => eval_expression(&**true_expr, component, local_context),
                Ok(false) => eval_expression(&**false_expr, component, local_context),
                _ => panic!("conditional expression did not evaluate to boolean"),
            }
        }
        Expression::Array { values, .. } => Value::Array(
            values.iter().map(|e| eval_expression(e, component, local_context)).collect(),
        ),
        Expression::Object { values, .. } => Value::Object(
            values
                .iter()
                .map(|(k, v)| (k.clone(), eval_expression(v, component, local_context)))
                .collect(),
        ),
        Expression::PathElements { elements } => {
            Value::PathElements(convert_path(elements, component, local_context))
        }
        Expression::StoreLocalVariable { name, value } => {
            let value = eval_expression(value, component, local_context);
            local_context.local_variables.insert(name.clone(), value);
            Value::Void
        }
        Expression::ReadLocalVariable { name, .. } => {
            local_context.local_variables.get(name).unwrap().clone()
        }
        Expression::EasingCurve(curve) => Value::EasingCurve(match curve {
            EasingCurve::Linear => corelib::animations::EasingCurve::Linear,
            EasingCurve::CubicBezier(a, b, c, d) => {
                corelib::animations::EasingCurve::CubicBezier([*a, *b, *c, *d])
            }
        }),
        Expression::EnumerationValue(value) => {
            Value::EnumerationValue(value.enumeration.name.clone(), value.to_string())
        }
    }
}

pub fn load_property(component: InstanceRef, element: &ElementRc, name: &str) -> Result<Value, ()> {
    generativity::make_guard!(guard);
    let enclosing_component = enclosing_component_for_element(&element, component, guard);
    let element = element.borrow();
    if element.id == element.enclosing_component.upgrade().unwrap().root_element.borrow().id {
        if let Some(x) = enclosing_component.component_type.custom_properties.get(name) {
            return unsafe {
                x.prop.get(Pin::new_unchecked(&*enclosing_component.as_ptr().add(x.offset)))
            };
        }
    };
    let item_info = enclosing_component
        .component_type
        .items
        .get(element.id.as_str())
        .unwrap_or_else(|| panic!("Unkown element for {}.{}", element.id, name));
    core::mem::drop(element);
    let item = unsafe { item_info.item_from_component(enclosing_component.as_ptr()) };
    Ok(item_info.rtti.properties.get(name).ok_or(())?.get(item))
}

pub fn store_property(
    component_instance: InstanceRef,
    element: &ElementRc,
    name: &str,
    value: Value,
) -> Result<(), ()> {
    generativity::make_guard!(guard);
    let enclosing_component = enclosing_component_for_element(&element, component_instance, guard);
    let maybe_animation = crate::dynamic_component::animation_for_property(
        enclosing_component,
        &element.borrow().property_animations,
        name,
    );

    let component = element.borrow().enclosing_component.upgrade().unwrap();
    if element.borrow().id == component.root_element.borrow().id {
        if let Some(x) = enclosing_component.component_type.custom_properties.get(name) {
            unsafe {
                let p = Pin::new_unchecked(&*enclosing_component.as_ptr().add(x.offset));
                return x.prop.set(p, value, maybe_animation);
            }
        }
    };
    let item_info = &enclosing_component.component_type.items[element.borrow().id.as_str()];
    let item = unsafe { item_info.item_from_component(enclosing_component.as_ptr()) };
    let p = &item_info.rtti.properties.get(name).ok_or(())?;
    p.set(item, value, maybe_animation);
    Ok(())
}

pub fn window_ref(component: InstanceRef) -> Option<sixtyfps_corelib::eventloop::ComponentWindow> {
    if let Some(parent_offset) = component.component_type.parent_component_offset {
        let parent_component =
            if let Some(parent) = parent_offset.apply(&*component.instance.as_ref()) {
                *parent
            } else {
                return None;
            };
        generativity::make_guard!(guard);
        window_ref(unsafe { InstanceRef::from_pin_ref(parent_component, guard) })
    } else {
        component
            .component_type
            .extra_data_offset
            .apply(&*component.instance.as_ref())
            .window
            .borrow()
            .as_ref()
            .map(|w| w.clone())
    }
}

pub fn enclosing_component_for_element<'a, 'old_id, 'new_id>(
    element: &'a ElementRc,
    component: InstanceRef<'a, 'old_id>,
    guard: generativity::Guard<'new_id>,
) -> InstanceRef<'a, 'new_id> {
    if Rc::ptr_eq(
        &element.borrow().enclosing_component.upgrade().unwrap(),
        &component.component_type.original,
    ) {
        // Safety: new_id is an unique id
        unsafe {
            std::mem::transmute::<InstanceRef<'a, 'old_id>, InstanceRef<'a, 'new_id>>(component)
        }
    } else {
        let parent_component = component
            .component_type
            .parent_component_offset
            .unwrap()
            .apply(component.as_ref())
            .unwrap();
        generativity::make_guard!(new_guard);
        let parent_instance = unsafe { InstanceRef::from_pin_ref(parent_component, new_guard) };
        let parent_instance = unsafe {
            core::mem::transmute::<InstanceRef, InstanceRef<'a, 'static>>(parent_instance)
        };
        enclosing_component_for_element(element, parent_instance, guard)
    }
}

pub fn new_struct_with_bindings<
    ElementType: 'static + Default + sixtyfps_corelib::rtti::BuiltinItem,
>(
    bindings: &HashMap<String, ExpressionSpanned>,
    component: InstanceRef,
    local_context: &mut EvalLocalContext,
) -> ElementType {
    let mut element = ElementType::default();
    for (prop, info) in ElementType::fields::<Value>().into_iter() {
        if let Some(binding) = &bindings.get(prop) {
            let value = eval_expression(&binding, component, local_context);
            info.set_field(&mut element, value).unwrap();
        }
    }
    element
}

fn convert_from_lyon_path<'a>(
    it: impl IntoIterator<Item = &'a lyon::path::Event<lyon::math::Point, lyon::math::Point>>,
) -> PathData {
    use lyon::path::Event;
    use sixtyfps_corelib::graphics::PathEvent;

    let mut coordinates = Vec::new();

    let events = it
        .into_iter()
        .map(|event| match event {
            Event::Begin { at } => {
                coordinates.push(at);
                PathEvent::Begin
            }
            Event::Line { from, to } => {
                coordinates.push(from);
                coordinates.push(to);
                PathEvent::Line
            }
            Event::Quadratic { from, ctrl, to } => {
                coordinates.push(from);
                coordinates.push(ctrl);
                coordinates.push(to);
                PathEvent::Quadratic
            }
            Event::Cubic { from, ctrl1, ctrl2, to } => {
                coordinates.push(from);
                coordinates.push(ctrl1);
                coordinates.push(ctrl2);
                coordinates.push(to);
                PathEvent::Cubic
            }
            Event::End { last, first, close } => {
                debug_assert_eq!(coordinates.first(), Some(&first));
                debug_assert_eq!(coordinates.last(), Some(&last));
                if *close {
                    PathEvent::EndClosed
                } else {
                    PathEvent::EndOpen
                }
            }
        })
        .collect::<Vec<_>>();

    PathData::Events(
        SharedArray::from(events.as_slice()),
        SharedArray::from_iter(coordinates.into_iter().cloned()),
    )
}

pub fn convert_path(
    path: &ExprPath,
    component: InstanceRef,
    local_context: &mut EvalLocalContext,
) -> PathData {
    match path {
        ExprPath::Elements(elements) => PathData::Elements(SharedArray::<PathElement>::from_iter(
            elements.iter().map(|element| convert_path_element(element, component, local_context)),
        )),
        ExprPath::Events(events) => convert_from_lyon_path(events.iter()),
    }
}

fn convert_path_element(
    expr_element: &ExprPathElement,
    component: InstanceRef,
    local_context: &mut EvalLocalContext,
) -> PathElement {
    match expr_element.element_type.native_class.class_name.as_str() {
        "LineTo" => PathElement::LineTo(new_struct_with_bindings(
            &expr_element.bindings,
            component,
            local_context,
        )),
        "ArcTo" => PathElement::ArcTo(new_struct_with_bindings(
            &expr_element.bindings,
            component,
            local_context,
        )),
        "Close" => PathElement::Close,
        _ => panic!(
            "Cannot create unsupported path element {}",
            expr_element.element_type.native_class.class_name
        ),
    }
}

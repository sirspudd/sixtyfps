/* LICENSE BEGIN
    This file is part of the SixtyFPS Project -- https://sixtyfps.io
    Copyright (c) 2020 Olivier Goffart <olivier.goffart@sixtyfps.io>
    Copyright (c) 2020 Simon Hausmann <simon.hausmann@sixtyfps.io>

    SPDX-License-Identifier: GPL-3.0-only
    This file is also available under commercial licensing terms.
    Please contact info@sixtyfps.io for more information.
LICENSE END */
//! This pass removes the property used in a two ways bindings

use crate::{
    diagnostics::BuildDiagnostics,
    expression_tree::{BuiltinFunction, Expression},
    object_tree::*,
};

pub fn determine_initial_focus_item(component: &Component, diag: &mut BuildDiagnostics) {
    fn find_initial_focus_item(component: &Component, _diag: &mut BuildDiagnostics) {
        recurse_elem(&component.root_element, &(), &mut |element_rc, _| {
            {
                let mut element = element_rc.borrow_mut();
                if let Some(initial_focus_binding) = element.bindings.remove("initial_focus") {
                    element.property_declarations.remove("initial_focus");
                    if let Expression::ElementReference(initial_focus_item) =
                        initial_focus_binding.expression
                    {
                        // ### check if already set
                        let setup_code = Expression::FunctionCall {
                            function: Box::new(Expression::BuiltinFunctionReference(
                                BuiltinFunction::SetFocusItem,
                            )),
                            arguments: vec![Expression::ElementReference(initial_focus_item)],
                        };

                        component.setup_code.borrow_mut().push(setup_code);
                    } else {
                    }
                }
            }
        });
    }

    find_initial_focus_item(component, diag);
}

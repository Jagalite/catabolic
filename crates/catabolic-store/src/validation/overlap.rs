// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Static field merging. Each selection-set entry compares local/inline fields,
//! then field/fragment and fragment/fragment pairs in source order. Fragment pair
//! caches belong to the entire document; strict comparisons subsume exclusive
//! ones. No directives or variables are evaluated by this rule.
use super::*;
use async_graphql_parser::types::Field;
use std::rc::Rc;

type SetId = usize;
type Search = Result<Vec<Conflict>, ()>;

struct FieldAt<'a> {
    parent: String,
    node: &'a Positioned<Field>,
    ty: Option<Value>,
    arguments: Vec<(String, String)>,
}
struct Fields<'a> {
    id: SetId,
    groups: Vec<(String, Vec<FieldAt<'a>>)>,
    indices: HashMap<String, usize>,
    fragments: Vec<String>,
}
struct Conflict {
    response: String,
    reason: String,
    left: Vec<Pos>,
    right: Vec<Pos>,
}

pub(super) struct Overlap<'a> {
    doc: &'a ExecutableDocument,
    tokens: &'a [Token],
    http: bool,
    fields: HashMap<SetId, Rc<Fields<'a>>>,
    field_fragment_pairs: HashMap<(SetId, String), bool>,
    fragment_pairs: HashMap<(String, String), bool>,
    comparisons: usize,
    exhausted: bool,
}

// graphql-core compares print_ast(sort_value_node(value)). Object order is
// immaterial, but list order, variables, number spelling and block-string kind
// remain significant. Located also retains duplicate object members.
fn argument_value(value: &Located<'_>) -> String {
    match value.value {
        Input::Object(_) => {
            let mut entries: Vec<_> = value.fields.iter().zip(&value.children).collect();
            entries.sort_by(|(a, _), (b, _)| a.0.cmp(&b.0));
            format!(
                "{{{}}}",
                entries
                    .into_iter()
                    .map(|((name, _), child)| { format!("{name}: {}", argument_value(child)) })
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
        Input::List(_) => format!(
            "[{}]",
            value
                .children
                .iter()
                .map(argument_value)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        _ => render_located(value),
    }
}
fn types_conflict(a: &Value, b: &Value) -> bool {
    for wrapper in ["LIST", "NON_NULL"] {
        if a["kind"] == wrapper {
            return b["kind"] != wrapper || types_conflict(&a["ofType"], &b["ofType"]);
        }
        if b["kind"] == wrapper {
            return true;
        }
    }
    (matches!(a["kind"].as_str(), Some("SCALAR" | "ENUM"))
        || matches!(b["kind"].as_str(), Some("SCALAR" | "ENUM")))
        && a["name"] != b["name"]
}
fn same_arguments(a: &[(String, String)], b: &[(String, String)]) -> bool {
    a.len() == b.len()
        && a.iter().all(|(name, value)| {
            // The frozen rule uses the last argument of each name on the right;
            // duplicate-argument validation is a separate source-ordered rule.
            b.iter()
                .rev()
                .find(|(other, _)| name == other)
                .is_some_and(|(_, other)| value == other)
        })
}

impl<'a> Overlap<'a> {
    pub(super) fn new(doc: &'a ExecutableDocument, tokens: &'a [Token], http: bool) -> Self {
        Self {
            doc,
            tokens,
            http,
            fields: HashMap::new(),
            field_fragment_pairs: HashMap::new(),
            fragment_pairs: HashMap::new(),
            comparisons: 0,
            exhausted: false,
        }
    }
    pub(super) fn check(
        &mut self,
        set: &'a Positioned<SelectionSet>,
        parent: &str,
    ) -> Vec<(String, Vec<Pos>)> {
        if self.exhausted {
            return vec![];
        }
        match self.within(set, parent) {
            Ok(conflicts) => conflicts.into_iter().take(21).map(|conflict| {
                let message = format!("Fields '{}' conflict because {}. Use different aliases on the fields to fetch both if this was intentional.", conflict.response, conflict.reason);
                (message, conflict.left.into_iter().chain(conflict.right).collect())
            }).collect(),
            Err(()) => {
                self.exhausted = true;
                vec![("Query is too complex to validate for overlapping fields: exceeded 250000 field comparisons. Validation was aborted.".into(), vec![set.pos])]
            }
        }
    }
    fn collect(&self, set: &'a Positioned<SelectionSet>, parent: &str, out: &mut Fields<'a>) {
        for selection in &set.node.items {
            match &selection.node {
                Selection::Field(node) => {
                    let response = node.node.response_key().node.to_string();
                    let index = *out.indices.entry(response.clone()).or_insert_with(|| {
                        out.groups.push((response, vec![]));
                        out.groups.len() - 1
                    });
                    // The overlap rule consults ordinary fields only, unlike
                    // TypeInfo's special handling of introspection meta-fields.
                    let ty = ty_for(parent, self.http)
                        .and_then(|ty| ty["fields"].as_array())
                        .and_then(|fields| {
                            fields
                                .iter()
                                .find(|f| f["name"] == node.node.name.node.as_str())
                        })
                        .map(|field| field["type"].clone());
                    let arguments = node
                        .node
                        .arguments
                        .iter()
                        .map(|(name, value)| {
                            let mut cursor = self.tokens.partition_point(|t| {
                                (t.pos.line, t.pos.column) < (value.pos.line, value.pos.column)
                            });
                            (
                                name.node.to_string(),
                                argument_value(&locate(
                                    &value.node,
                                    self.tokens,
                                    &mut cursor,
                                    value.pos,
                                )),
                            )
                        })
                        .collect();
                    out.groups[index].1.push(FieldAt {
                        parent: parent.into(),
                        node,
                        ty,
                        arguments,
                    });
                }
                Selection::InlineFragment(fragment) => self.collect(
                    &fragment.node.selection_set,
                    fragment
                        .node
                        .type_condition
                        .as_ref()
                        .map_or(parent, |c| c.node.on.node.as_str()),
                    out,
                ),
                Selection::FragmentSpread(spread) => {
                    let name = spread.node.fragment_name.node.to_string();
                    if !out.fragments.contains(&name) {
                        out.fragments.push(name);
                    }
                }
            }
        }
    }
    fn fields(&mut self, set: &'a Positioned<SelectionSet>, parent: &str) -> Rc<Fields<'a>> {
        let id = std::ptr::from_ref(set) as usize;
        if let Some(fields) = self.fields.get(&id) {
            return fields.clone();
        }
        let mut fields = Fields {
            id,
            groups: vec![],
            indices: HashMap::new(),
            fragments: vec![],
        };
        self.collect(set, parent, &mut fields);
        let fields = Rc::new(fields);
        self.fields.insert(id, fields.clone());
        fields
    }
    fn fragment_fields(&mut self, name: &str) -> Option<Rc<Fields<'a>>> {
        let fragment = self.doc.fragments.get(name)?;
        Some(self.fields(
            &fragment.node.selection_set,
            fragment.node.type_condition.node.on.node.as_str(),
        ))
    }
    fn within(&mut self, set: &'a Positioned<SelectionSet>, parent: &str) -> Search {
        let fields = self.fields(set, parent);
        let mut conflicts = vec![];
        for (response, group) in &fields.groups {
            for (i, a) in group.iter().enumerate() {
                for b in &group[i + 1..] {
                    if let Some(conflict) = self.compare(response, a, b, false)? {
                        conflicts.push(conflict);
                    }
                }
            }
        }
        for (i, name) in fields.fragments.iter().enumerate() {
            self.fields_fragment(&mut conflicts, &fields, name, false)?;
            for other in &fields.fragments[i + 1..] {
                self.fragments(&mut conflicts, name, other, false)?;
            }
        }
        Ok(conflicts)
    }
    fn between(
        &mut self,
        out: &mut Vec<Conflict>,
        a: &Fields<'a>,
        b: &Fields<'a>,
        exclusive: bool,
    ) -> Result<(), ()> {
        for (response, left) in &a.groups {
            if let Some(index) = b.indices.get(response) {
                for first in left {
                    for second in &b.groups[*index].1 {
                        if let Some(conflict) = self.compare(response, first, second, exclusive)? {
                            out.push(conflict);
                        }
                    }
                }
            }
        }
        Ok(())
    }
    fn fields_fragment(
        &mut self,
        out: &mut Vec<Conflict>,
        fields: &Fields<'a>,
        name: &str,
        exclusive: bool,
    ) -> Result<(), ()> {
        let key = (fields.id, name.to_owned());
        if self
            .field_fragment_pairs
            .get(&key)
            .is_some_and(|old| exclusive || !old)
        {
            return Ok(());
        }
        self.field_fragment_pairs.insert(key, exclusive);
        let Some(other) = self.fragment_fields(name) else {
            return Ok(());
        };
        if fields.id == other.id {
            return Ok(());
        }
        self.between(out, fields, &other, exclusive)?;
        for name in &other.fragments {
            self.fields_fragment(out, fields, name, exclusive)?;
        }
        Ok(())
    }
    fn fragments(
        &mut self,
        out: &mut Vec<Conflict>,
        a: &str,
        b: &str,
        exclusive: bool,
    ) -> Result<(), ()> {
        if a == b {
            return Ok(());
        }
        let key = if a < b {
            (a.to_owned(), b.to_owned())
        } else {
            (b.to_owned(), a.to_owned())
        };
        if self
            .fragment_pairs
            .get(&key)
            .is_some_and(|old| exclusive || !old)
        {
            return Ok(());
        }
        self.fragment_pairs.insert(key, exclusive);
        let (Some(first), Some(second)) = (self.fragment_fields(a), self.fragment_fields(b)) else {
            return Ok(());
        };
        self.between(out, &first, &second, exclusive)?;
        for name in &second.fragments {
            self.fragments(out, a, name, exclusive)?;
        }
        for name in &first.fragments {
            self.fragments(out, name, b, exclusive)?;
        }
        Ok(())
    }
    fn subfields(&mut self, a: &FieldAt<'a>, b: &FieldAt<'a>, exclusive: bool) -> Search {
        let first = self.fields(&a.node.node.selection_set, a.ty.as_ref().map_or("", named));
        let second = self.fields(&b.node.node.selection_set, b.ty.as_ref().map_or("", named));
        let mut out = vec![];
        self.between(&mut out, &first, &second, exclusive)?;
        for name in &second.fragments {
            self.fields_fragment(&mut out, &first, name, exclusive)?;
        }
        for name in &first.fragments {
            self.fields_fragment(&mut out, &second, name, exclusive)?;
        }
        for a in &first.fragments {
            for b in &second.fragments {
                self.fragments(&mut out, a, b, exclusive)?;
            }
        }
        Ok(out)
    }
    fn compare(
        &mut self,
        response: &str,
        a: &FieldAt<'a>,
        b: &FieldAt<'a>,
        exclusive: bool,
    ) -> Result<Option<Conflict>, ()> {
        self.comparisons += 1;
        if self.comparisons > 250_000 {
            return Err(());
        }
        let exclusive = exclusive
            || a.parent != b.parent
                && ty_for(&a.parent, self.http).is_some_and(|t| t["kind"] == "OBJECT")
                && ty_for(&b.parent, self.http).is_some_and(|t| t["kind"] == "OBJECT");
        let name1 = &a.node.node.name.node;
        let name2 = &b.node.node.name.node;
        let reason = if !exclusive && name1 != name2 {
            Some(format!("'{name1}' and '{name2}' are different fields"))
        } else if !exclusive && !same_arguments(&a.arguments, &b.arguments) {
            Some("they have differing arguments".into())
        } else if let (Some(first), Some(second)) = (&a.ty, &b.ty)
            && types_conflict(first, second)
        {
            Some(format!(
                "they return conflicting types '{}' and '{}'",
                display(first),
                display(second)
            ))
        } else {
            None
        };
        if let Some(reason) = reason {
            return Ok(Some(Conflict {
                response: response.into(),
                reason,
                left: vec![a.node.pos],
                right: vec![b.node.pos],
            }));
        }
        if !a.node.node.selection_set.node.items.is_empty()
            && !b.node.node.selection_set.node.items.is_empty()
        {
            let conflicts = self.subfields(a, b, exclusive)?;
            if !conflicts.is_empty() {
                let reason = conflicts
                    .iter()
                    .map(|c| format!("subfields '{}' conflict because {}", c.response, c.reason))
                    .collect::<Vec<_>>()
                    .join(" and ");
                let mut left = vec![a.node.pos];
                let mut right = vec![b.node.pos];
                for conflict in conflicts {
                    left.extend(conflict.left);
                    right.extend(conflict.right);
                }
                return Ok(Some(Conflict {
                    response: response.into(),
                    reason,
                    left,
                    right,
                }));
            }
        }
        Ok(None)
    }
}

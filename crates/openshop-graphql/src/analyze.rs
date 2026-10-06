//! Checks a GraphQL operation against a schema and describes the shape of
//! its variables and of its result.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use graphql_parser::Pos;
use graphql_parser::query as ast;

use crate::schema::{Field, InputValue, Schema, TypeDef, TypeRef};

type Document<'a> = ast::Document<'a, String>;
type SelectionSet<'a> = ast::SelectionSet<'a, String>;
type Value<'a> = ast::Value<'a, String>;
type Directive<'a> = ast::Directive<'a, String>;

/// An operation that does not parse or does not fit the schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub message: String,
    /// Line and column in the operation text, both starting at 1.
    pub position: Option<(usize, usize)>,
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.position {
            Some((line, column)) => write!(formatter, "{} (line {line}, column {column})", self.message),
            None => formatter.write_str(&self.message),
        }
    }
}

impl std::error::Error for Error {}

type Result<T> = std::result::Result<T, Error>;

fn error_at<T>(position: Pos, message: impl Into<String>) -> Result<T> {
    Err(Error {
        message: message.into(),
        position: Some((position.line, position.column)),
    })
}

fn error<T>(message: impl Into<String>) -> Result<T> {
    Err(Error {
        message: message.into(),
        position: None,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationKind {
    Query,
    Mutation,
}

/// How a scalar travels in JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scalar {
    /// `String`, `ID`, enum values, and Shopify's string-encoded scalars such
    /// as `DateTime`, `Decimal`, `Money`, `URL` and `UnsignedInt64`.
    String,
    Int,
    Float,
    Boolean,
    /// `JSON`: any value.
    Json,
}

impl Scalar {
    fn of(name: &str) -> Self {
        match name {
            "Int" => Self::Int,
            "Float" => Self::Float,
            "Boolean" => Self::Boolean,
            "JSON" => Self::Json,
            _ => Self::String,
        }
    }
}

/// What a variable holds, lists and nullability set aside.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VariableBase {
    Scalar(Scalar),
    Enum,
    /// An input object, named by the variable's type.
    Input,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Variable {
    pub name: String,
    pub ty: TypeRef,
    pub has_default: bool,
    pub base: VariableBase,
}

impl Variable {
    /// The operation cannot run without a value for this variable.
    pub fn is_required(&self) -> bool {
        self.ty.is_non_null() && !self.has_default
    }
}

/// The fields selected on an object, interface or union.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub type_name: String,
    pub fields: Vec<SelectedField>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedField {
    /// The key of the field in the response: its alias, or its name.
    pub key: String,
    pub ty: TypeRef,
    /// The field may be absent: it is selected in a fragment that does not
    /// always apply, or under `@include` / `@skip`.
    pub conditional: bool,
    pub base: FieldBase,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldBase {
    Scalar(Scalar),
    Object(Selection),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Operation {
    pub kind: OperationKind,
    pub name: Option<String>,
    pub variables: Vec<Variable>,
    pub data: Selection,
}

/// Parses `source`, checks it against `schema` and returns its shape.
///
/// `source` contains exactly one query or mutation, and the fragments it uses.
pub fn analyze(schema: &Schema, source: &str) -> Result<Operation> {
    let document: Document = match graphql_parser::parse_query(source) {
        Ok(document) => document,
        Err(parse_error) => {
            let text = parse_error.to_string();
            let detail = text.strip_prefix("query parse error: ").unwrap_or(&text);
            return error(format!("Invalid GraphQL: {}", detail.trim().replace('\n', " ")));
        }
    };

    let mut operations = Vec::new();
    let mut fragments = BTreeMap::new();
    for definition in &document.definitions {
        match definition {
            ast::Definition::Operation(operation) => operations.push(operation),
            ast::Definition::Fragment(fragment) => {
                if fragments.insert(fragment.name.as_str(), fragment).is_some() {
                    return error_at(
                        fragment.position,
                        format!("Fragment `{}` is defined twice", fragment.name),
                    );
                }
            }
        }
    }
    let operation = match operations.as_slice() {
        [operation] => *operation,
        [] => return error("The document contains no query or mutation"),
        [_, second, ..] => {
            return error_at(
                operation_position(second),
                "The document contains several operations; `graphql!` takes one",
            );
        }
    };

    let (kind, name, definitions, directives, selection_set, position) = match operation {
        ast::OperationDefinition::SelectionSet(set) => (OperationKind::Query, None, &[][..], &[][..], set, set.span.0),
        ast::OperationDefinition::Query(query) => (
            OperationKind::Query,
            query.name.clone(),
            &query.variable_definitions[..],
            &query.directives[..],
            &query.selection_set,
            query.position,
        ),
        ast::OperationDefinition::Mutation(mutation) => (
            OperationKind::Mutation,
            mutation.name.clone(),
            &mutation.variable_definitions[..],
            &mutation.directives[..],
            &mutation.selection_set,
            mutation.position,
        ),
        ast::OperationDefinition::Subscription(subscription) => {
            return error_at(subscription.position, "Subscriptions are not supported");
        }
    };
    let root = match kind {
        OperationKind::Query => schema.query.as_str(),
        OperationKind::Mutation => match &schema.mutation {
            Some(mutation) => mutation.as_str(),
            None => return error_at(position, "The schema has no mutations"),
        },
    };

    let mut analyzer = Analyzer {
        schema,
        fragments,
        variables: BTreeMap::new(),
        used_variables: BTreeSet::new(),
        used_fragments: BTreeSet::new(),
        fragment_stack: Vec::new(),
    };

    let mut variables = Vec::new();
    for definition in definitions {
        let ty = type_ref(&definition.var_type);
        let base = match schema.get(ty.name()) {
            Some(TypeDef::Scalar) => VariableBase::Scalar(Scalar::of(ty.name())),
            Some(TypeDef::Enum { .. }) => VariableBase::Enum,
            Some(TypeDef::Input { .. }) => VariableBase::Input,
            Some(other) => {
                return error_at(
                    definition.position,
                    format!(
                        "Variable `${}` cannot have the type `{ty}`: `{}` is {}, not an input type",
                        definition.name,
                        ty.name(),
                        other.kind_name()
                    ),
                );
            }
            None => {
                return error_at(
                    definition.position,
                    format!(
                        "Variable `${}` has the unknown type `{}`{}",
                        definition.name,
                        ty.name(),
                        did_you_mean(ty.name(), schema.types.keys().map(String::as_str))
                    ),
                );
            }
        };
        let variable = Variable {
            name: definition.name.clone(),
            ty,
            has_default: definition.default_value.is_some(),
            base,
        };
        if analyzer
            .variables
            .insert(variable.name.clone(), variable.clone())
            .is_some()
        {
            return error_at(
                definition.position,
                format!("Variable `${}` is declared twice", variable.name),
            );
        }
        variables.push(variable);
    }

    analyzer.check_directives(directives)?;
    let data = analyzer.selection(root, &[selection_set], position)?;

    if let Some(unused) = variables
        .iter()
        .find(|variable| !analyzer.used_variables.contains(&variable.name))
    {
        return error(format!("Variable `${}` is declared but never used", unused.name));
    }
    if let Some((unused, fragment)) = analyzer
        .fragments
        .iter()
        .find(|(name, _)| !analyzer.used_fragments.contains(**name))
    {
        return error_at(
            fragment.position,
            format!("Fragment `{unused}` is defined but never used"),
        );
    }

    Ok(Operation {
        kind,
        name,
        variables,
        data,
    })
}

fn operation_position(operation: &ast::OperationDefinition<'_, String>) -> Pos {
    match operation {
        ast::OperationDefinition::SelectionSet(set) => set.span.0,
        ast::OperationDefinition::Query(query) => query.position,
        ast::OperationDefinition::Mutation(mutation) => mutation.position,
        ast::OperationDefinition::Subscription(subscription) => subscription.position,
    }
}

fn type_ref(ty: &ast::Type<'_, String>) -> TypeRef {
    match ty {
        ast::Type::NamedType(name) => TypeRef::Named(name.clone()),
        ast::Type::ListType(inner) => TypeRef::List(Box::new(type_ref(inner))),
        ast::Type::NonNullType(inner) => TypeRef::NonNull(Box::new(type_ref(inner))),
    }
}

/// A field being assembled from every place that selects its response key.
struct Pending<'a> {
    key: String,
    field: String,
    ty: TypeRef,
    conditional: bool,
    sets: Vec<&'a SelectionSet<'a>>,
    position: Pos,
}

struct Analyzer<'a> {
    schema: &'a Schema,
    fragments: BTreeMap<&'a str, &'a ast::FragmentDefinition<'a, String>>,
    variables: BTreeMap<String, Variable>,
    used_variables: BTreeSet<String>,
    used_fragments: BTreeSet<&'a str>,
    fragment_stack: Vec<&'a str>,
}

impl<'a> Analyzer<'a> {
    /// Resolves the merged selection sets of one value of type `parent`.
    fn selection(&mut self, parent: &str, sets: &[&'a SelectionSet<'a>], position: Pos) -> Result<Selection> {
        let mut pending = Vec::new();
        for set in sets {
            self.collect(parent, set, false, &mut pending)?;
        }
        if pending.is_empty() {
            return error_at(position, format!("The selection on `{parent}` is empty"));
        }

        let mut fields = Vec::new();
        for field in pending {
            let named = field.ty.name();
            let base = match self.schema.get(named) {
                Some(definition) if definition.is_composite() => {
                    if field.sets.is_empty() {
                        return error_at(
                            field.position,
                            format!(
                                "Field `{}` has the type `{}`: select the subfields you need, `{} {{ … }}`",
                                field.field, field.ty, field.field
                            ),
                        );
                    }
                    FieldBase::Object(self.selection(named, &field.sets, field.position)?)
                }
                Some(_) => {
                    if !field.sets.is_empty() {
                        return error_at(
                            field.position,
                            format!(
                                "Field `{}` has the type `{}`, which has no subfields",
                                field.field, field.ty
                            ),
                        );
                    }
                    FieldBase::Scalar(Scalar::of(named))
                }
                None => {
                    return error_at(field.position, format!("The schema does not define the type `{named}`"));
                }
            };
            fields.push(SelectedField {
                key: field.key,
                ty: field.ty,
                conditional: field.conditional,
                base,
            });
        }

        Ok(Selection {
            type_name: parent.to_string(),
            fields,
        })
    }

    fn collect(
        &mut self,
        parent: &str,
        set: &'a SelectionSet<'a>,
        conditional: bool,
        pending: &mut Vec<Pending<'a>>,
    ) -> Result<()> {
        for item in &set.items {
            match item {
                ast::Selection::Field(field) => {
                    let conditional = conditional | self.check_directives(&field.directives)?;
                    let ty = self.field_type(parent, field)?;
                    let key = field.alias.as_ref().unwrap_or(&field.name).clone();
                    let sets = (!field.selection_set.items.is_empty()).then_some(&field.selection_set);

                    match pending.iter_mut().find(|existing| existing.key == key) {
                        Some(existing) => {
                            if existing.field != field.name || existing.ty != ty {
                                return error_at(
                                    field.position,
                                    format!(
                                        "`{key}` is selected twice with different fields or types (`{}: {}` and `{}: {ty}`); give one of them an alias",
                                        existing.field, existing.ty, field.name
                                    ),
                                );
                            }
                            existing.conditional &= conditional;
                            existing.sets.extend(sets);
                        }
                        None => pending.push(Pending {
                            key,
                            field: field.name.clone(),
                            ty,
                            conditional,
                            sets: sets.into_iter().collect(),
                            position: field.position,
                        }),
                    }
                }
                ast::Selection::InlineFragment(fragment) => {
                    let conditional = conditional | self.check_directives(&fragment.directives)?;
                    let (target, always) = match &fragment.type_condition {
                        Some(ast::TypeCondition::On(condition)) => (
                            condition.as_str(),
                            self.fragment_always_applies(parent, condition, fragment.position)?,
                        ),
                        None => (parent, true),
                    };
                    self.collect(target, &fragment.selection_set, conditional | !always, pending)?;
                }
                ast::Selection::FragmentSpread(spread) => {
                    let conditional = conditional | self.check_directives(&spread.directives)?;
                    let name = spread.fragment_name.as_str();
                    let Some(fragment) = self.fragments.get(name).copied() else {
                        return error_at(
                            spread.position,
                            format!(
                                "Fragment `{name}` is not defined{}; a `graphql!` document contains the fragments it uses",
                                did_you_mean(name, self.fragments.keys().copied())
                            ),
                        );
                    };
                    if self.fragment_stack.contains(&name) {
                        return error_at(spread.position, format!("Fragment `{name}` includes itself"));
                    }
                    self.used_fragments.insert(fragment.name.as_str());
                    self.check_directives(&fragment.directives)?;

                    let ast::TypeCondition::On(condition) = &fragment.type_condition;
                    let always = self.fragment_always_applies(parent, condition, spread.position)?;
                    self.fragment_stack.push(fragment.name.as_str());
                    let collected = self.collect(condition, &fragment.selection_set, conditional | !always, pending);
                    self.fragment_stack.pop();
                    collected?;
                }
            }
        }
        Ok(())
    }

    /// Whether every value of type `parent` matches a fragment on `condition`.
    /// Fails when none can.
    fn fragment_always_applies(&self, parent: &str, condition: &str, position: Pos) -> Result<bool> {
        match self.schema.get(condition) {
            Some(definition) if definition.is_composite() => {}
            Some(definition) => {
                return error_at(
                    position,
                    format!(
                        "A fragment cannot be on `{condition}`, which is {}",
                        definition.kind_name()
                    ),
                );
            }
            None => {
                return error_at(
                    position,
                    format!(
                        "A fragment is on the unknown type `{condition}`{}",
                        did_you_mean(condition, self.schema.possible_types(parent).into_iter())
                    ),
                );
            }
        }
        let of_parent = self.schema.possible_types(parent);
        let of_condition = self.schema.possible_types(condition);
        if !of_parent.iter().any(|ty| of_condition.contains(ty)) {
            return error_at(
                position,
                format!("A fragment on `{condition}` never applies to a `{parent}`"),
            );
        }
        Ok(of_parent.iter().all(|ty| of_condition.contains(ty)))
    }

    /// The type of `field` on `parent`, after checking its arguments.
    fn field_type(&mut self, parent: &str, field: &'a ast::Field<'a, String>) -> Result<TypeRef> {
        if field.name == "__typename" {
            if let Some((argument, _)) = field.arguments.first() {
                return error_at(
                    field.position,
                    format!("Field `__typename` has no argument `{argument}`"),
                );
            }
            return Ok(TypeRef::NonNull(Box::new(TypeRef::Named("String".to_string()))));
        }

        let fields = match self.schema.get(parent) {
            Some(TypeDef::Object { fields, .. } | TypeDef::Interface { fields, .. }) => fields,
            Some(TypeDef::Union { possible_types }) => {
                return error_at(
                    field.position,
                    format!(
                        "`{parent}` is a union: select `{}` inside a fragment such as `... on {} {{ {} }}`",
                        field.name,
                        possible_types.first().map_or("Type", String::as_str),
                        field.name
                    ),
                );
            }
            _ => {
                return error_at(field.position, format!("`{parent}` has no fields to select"));
            }
        };
        let Some(definition) = fields.get(&field.name) else {
            return error_at(
                field.position,
                format!(
                    "Type `{parent}` has no field `{}`{}",
                    field.name,
                    did_you_mean(&field.name, fields.keys().map(String::as_str))
                ),
            );
        };
        self.check_arguments(parent, field, definition)?;
        Ok(definition.ty.clone())
    }

    fn check_arguments(&mut self, parent: &str, field: &'a ast::Field<'a, String>, definition: &Field) -> Result<()> {
        for (name, value) in &field.arguments {
            let Some(argument) = definition.args.get(name) else {
                return error_at(
                    field.position,
                    format!(
                        "Field `{parent}.{}` has no argument `{name}`{}",
                        field.name,
                        did_you_mean(name, definition.args.keys().map(String::as_str))
                    ),
                );
            };
            let place = format!("argument `{name}` of `{parent}.{}`", field.name);
            self.check_value(value, argument, &place, field.position)?;
        }
        for (name, argument) in &definition.args {
            if argument.is_required() && !field.arguments.iter().any(|(given, _)| given == name) {
                return error_at(
                    field.position,
                    format!(
                        "Field `{parent}.{}` requires the argument `{name}: {}`",
                        field.name, argument.ty
                    ),
                );
            }
        }
        Ok(())
    }

    /// Checks directive arguments. Returns whether the directives make the
    /// selection conditional.
    fn check_directives(&mut self, directives: &'a [Directive<'a>]) -> Result<bool> {
        let mut conditional = false;
        let condition = InputValue {
            ty: TypeRef::NonNull(Box::new(TypeRef::Named("Boolean".to_string()))),
            has_default: false,
        };
        for directive in directives {
            let is_condition = matches!(directive.name.as_str(), "include" | "skip");
            conditional |= is_condition;
            for (name, value) in &directive.arguments {
                if is_condition && name == "if" {
                    let place = format!("argument `if` of `@{}`", directive.name);
                    self.check_value(value, &condition, &place, directive.position)?;
                } else {
                    self.mark_variables(value, directive.position)?;
                }
            }
            if is_condition && !directive.arguments.iter().any(|(name, _)| name == "if") {
                return error_at(
                    directive.position,
                    format!("`@{}` requires the argument `if: Boolean!`", directive.name),
                );
            }
        }
        Ok(conditional)
    }

    /// Marks the variables of a value whose type is not known.
    fn mark_variables(&mut self, value: &Value<'a>, position: Pos) -> Result<()> {
        match value {
            ast::Value::Variable(name) => {
                self.variable(name, position)?;
            }
            ast::Value::List(items) => {
                for item in items {
                    self.mark_variables(item, position)?;
                }
            }
            ast::Value::Object(fields) => {
                for item in fields.values() {
                    self.mark_variables(item, position)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn variable(&mut self, name: &str, position: Pos) -> Result<Variable> {
        let Some(variable) = self.variables.get(name).cloned() else {
            return error_at(
                position,
                format!(
                    "Variable `${name}` is not declared{}",
                    did_you_mean(name, self.variables.keys().map(String::as_str))
                ),
            );
        };
        self.used_variables.insert(name.to_string());
        Ok(variable)
    }

    /// Checks a literal or a variable given where `expected` is wanted.
    fn check_value(&mut self, value: &Value<'a>, expected: &InputValue, place: &str, position: Pos) -> Result<()> {
        let inner = expected.ty.nullable();
        match value {
            ast::Value::Variable(name) => {
                let variable = self.variable(name, position)?;
                if !variable_fits(&variable, expected) {
                    return error_at(
                        position,
                        format!(
                            "Variable `${name}` of type `{}` cannot be used for {place}, which expects `{}`",
                            variable.ty, expected.ty
                        ),
                    );
                }
            }
            ast::Value::Null => {
                if expected.ty.is_non_null() {
                    return error_at(
                        position,
                        format!("`null` is not allowed for {place}, of type `{}`", expected.ty),
                    );
                }
            }
            ast::Value::List(items) => {
                // GraphQL accepts a single value where a list is expected, not the opposite.
                let TypeRef::List(item_type) = inner else {
                    return error_at(
                        position,
                        format!("A list is given for {place}, of type `{}`", expected.ty),
                    );
                };
                let item = InputValue {
                    ty: (**item_type).clone(),
                    has_default: false,
                };
                for value in items {
                    self.check_value(value, &item, place, position)?;
                }
            }
            ast::Value::Object(given) => {
                let named = inner.name();
                let Some(TypeDef::Input { fields }) = self.schema.get(named) else {
                    return error_at(
                        position,
                        format!("An object is given for {place}, of type `{}`", expected.ty),
                    );
                };
                for (name, value) in given {
                    let Some(field) = fields.get(name) else {
                        return error_at(
                            position,
                            format!(
                                "Input type `{named}` has no field `{name}`{} (in {place})",
                                did_you_mean(name, fields.keys().map(String::as_str))
                            ),
                        );
                    };
                    self.check_value(value, field, &format!("field `{name}` of `{named}`"), position)?;
                }
                if let Some((missing, field)) = fields
                    .iter()
                    .find(|(name, field)| field.is_required() && !given.contains_key(*name))
                {
                    return error_at(
                        position,
                        format!(
                            "Input type `{named}` requires the field `{missing}: {}` (in {place})",
                            field.ty
                        ),
                    );
                }
            }
            ast::Value::Enum(given) => match self.schema.get(inner.name()) {
                Some(TypeDef::Enum { values }) if matches!(inner, TypeRef::Named(_)) => {
                    if !values.contains(given) {
                        return error_at(
                            position,
                            format!(
                                "`{given}` is not a value of the enum `{}`{} (in {place})",
                                inner.name(),
                                did_you_mean(given, values.iter().map(String::as_str))
                            ),
                        );
                    }
                }
                // A single value where a list of enum values is expected.
                Some(TypeDef::Enum { .. }) => {
                    let item = InputValue {
                        ty: TypeRef::Named(inner.name().to_string()),
                        has_default: false,
                    };
                    self.check_value(value, &item, place, position)?;
                }
                _ => {
                    return error_at(
                        position,
                        format!(
                            "The enum value `{given}` is given for {place}, of type `{}`",
                            expected.ty
                        ),
                    );
                }
            },
            literal => {
                let given = match literal {
                    ast::Value::Int(_) => "an integer",
                    ast::Value::Float(_) => "a float",
                    ast::Value::String(_) => "a string",
                    _ => "a boolean",
                };
                let accepted = match (inner.name(), literal) {
                    ("Int", ast::Value::Int(_)) => true,
                    ("Float", ast::Value::Int(_) | ast::Value::Float(_)) => true,
                    ("String", ast::Value::String(_)) => true,
                    ("Boolean", ast::Value::Boolean(_)) => true,
                    ("ID", ast::Value::String(_) | ast::Value::Int(_)) => true,
                    ("Int" | "Float" | "String" | "Boolean" | "ID", _) => false,
                    // Custom scalars define their own literals; enums and input objects do not accept these.
                    (name, _) => matches!(self.schema.get(name), Some(TypeDef::Scalar)),
                };
                if !accepted {
                    return error_at(
                        position,
                        format!("{given} is given for {place}, of type `{}`", expected.ty),
                    );
                }
            }
        }
        Ok(())
    }
}

/// The rule of the GraphQL specification for using a variable at a location.
fn variable_fits(variable: &Variable, location: &InputValue) -> bool {
    fn compatible(variable: &TypeRef, location: &TypeRef) -> bool {
        match (variable, location) {
            (TypeRef::NonNull(variable), TypeRef::NonNull(location)) => compatible(variable, location),
            (_, TypeRef::NonNull(_)) => false,
            (TypeRef::NonNull(variable), location) => compatible(variable, location),
            (TypeRef::List(variable), TypeRef::List(location)) => compatible(variable, location),
            (TypeRef::Named(variable), TypeRef::Named(location)) => variable == location,
            _ => false,
        }
    }
    if location.ty.is_non_null() && !variable.ty.is_non_null() {
        // A nullable variable is accepted when a default value fills the gap.
        return (variable.has_default || location.has_default) && compatible(&variable.ty, location.ty.nullable());
    }
    compatible(&variable.ty, &location.ty)
}

/// ` (did you mean `x`?)` for the candidate closest to `name`, or nothing.
fn did_you_mean<'a>(name: &str, candidates: impl Iterator<Item = &'a str>) -> String {
    let lowered = name.to_lowercase();
    // Half of the name may differ, within three edits: `frist` → `first`, `CREATED` → `CREATED_AT`.
    let limit = (name.len() / 2).clamp(1, 3);
    candidates
        .map(|candidate| (edit_distance(&lowered, &candidate.to_lowercase()), candidate))
        .filter(|(distance, _)| *distance <= limit)
        .min()
        .map(|(_, candidate)| format!(" (did you mean `{candidate}`?)"))
        .unwrap_or_default()
}

fn edit_distance(left: &str, right: &str) -> usize {
    let right: Vec<char> = right.chars().collect();
    let mut row: Vec<usize> = (0..=right.len()).collect();
    for (i, a) in left.chars().enumerate() {
        let mut previous = row[0];
        row[0] = i + 1;
        for (j, b) in right.iter().enumerate() {
            let substitution = previous + usize::from(a != *b);
            previous = row[j + 1];
            row[j + 1] = substitution.min(previous + 1).min(row[j] + 1);
        }
    }
    row[right.len()]
}

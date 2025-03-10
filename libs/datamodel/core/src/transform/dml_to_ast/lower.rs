use crate::common::preview_features::PreviewFeature;
use crate::{ast, dml, Datasource, IndexField, PrimaryKeyField, SortOrder};
use enumflags2::BitFlags;

pub struct LowerDmlToAst<'a> {
    pub datasource: Option<&'a Datasource>,
    pub preview_features: BitFlags<PreviewFeature>,
}

impl<'a> LowerDmlToAst<'a> {
    /// Creates a new instance, with all builtin attributes registered.
    pub fn new(datasource: Option<&'a Datasource>, preview_features: BitFlags<PreviewFeature>) -> Self {
        Self {
            datasource,
            preview_features,
        }
    }

    pub fn lower(&self, datamodel: &dml::Datamodel) -> ast::SchemaAst {
        let mut tops: Vec<ast::Top> = Vec::new();

        for r#type in datamodel.composite_types.iter() {
            tops.push(ast::Top::CompositeType(self.lower_composite_type(r#type)));
        }

        for model in datamodel.models() {
            if !model.is_generated {
                tops.push(ast::Top::Model(self.lower_model(model, datamodel)))
            }
        }

        for enm in datamodel.enums() {
            tops.push(ast::Top::Enum(self.lower_enum(enm)))
        }

        ast::SchemaAst { tops }
    }

    pub fn lower_model(&self, model: &dml::Model, datamodel: &dml::Datamodel) -> ast::Model {
        let mut fields: Vec<ast::Field> = Vec::new();

        for field in model.fields() {
            fields.push(self.lower_field(model, field, datamodel))
        }

        ast::Model {
            name: ast::Identifier::new(&model.name),
            fields,
            attributes: self.lower_model_attributes(model),
            documentation: model.documentation.clone().map(|text| ast::Comment { text }),
            span: ast::Span::empty(),
            commented_out: model.is_commented_out,
        }
    }

    fn lower_enum(&self, enm: &dml::Enum) -> ast::Enum {
        ast::Enum {
            name: ast::Identifier::new(&enm.name),
            values: enm
                .values()
                .map(|v| ast::EnumValue {
                    name: ast::Identifier::new(&v.name),
                    attributes: self.lower_enum_value_attributes(v),
                    documentation: v.documentation.clone().map(|text| ast::Comment { text }),
                    span: ast::Span::empty(),
                    commented_out: v.commented_out,
                })
                .collect(),
            attributes: self.lower_enum_attributes(enm),
            documentation: enm.documentation.clone().map(|text| ast::Comment { text }),
            span: ast::Span::empty(),
        }
    }

    pub fn lower_field(&self, model: &dml::Model, field: &dml::Field, datamodel: &dml::Datamodel) -> ast::Field {
        let mut attributes = self.lower_field_attributes(model, field, datamodel);

        let native_type = field.as_scalar_field().and_then(|sf| sf.field_type.as_native_type());

        if let (Some((scalar_type, native_type)), Some(datasource)) = (native_type, self.datasource) {
            self.lower_native_type_attribute(scalar_type, native_type, &mut attributes, datasource);
        }

        ast::Field {
            name: ast::Identifier::new(field.name()),
            arity: self.lower_field_arity(field.arity()),
            attributes,
            field_type: self.lower_type(&field.field_type()),
            documentation: field.documentation().map(|text| ast::Comment { text: text.to_owned() }),
            span: ast::Span::empty(),
            is_commented_out: field.is_commented_out(),
        }
    }

    pub(super) fn lower_composite_type(&self, r#type: &dml::CompositeType) -> ast::CompositeType {
        let mut fields: Vec<ast::Field> = Vec::new();

        for field in r#type.fields.iter() {
            let mut attributes = field
                .database_name
                .as_ref()
                .map(|db_name| {
                    vec![ast::Attribute::new(
                        "map",
                        vec![ast::Argument::new_unnamed(ast::Expression::StringValue(
                            String::from(db_name),
                            ast::Span::empty(),
                        ))],
                    )]
                })
                .unwrap_or_else(Vec::new);

            let native_type = field.r#type.as_native_type();

            if let (Some((scalar_type, native_type)), Some(datasource)) = (native_type, self.datasource) {
                self.lower_native_type_attribute(scalar_type, native_type, &mut attributes, datasource);
            }

            fields.push(ast::Field {
                field_type: self.lower_composite_field_type(&field.r#type),
                name: ast::Identifier::new(&field.name),
                arity: self.lower_field_arity(&field.arity),
                attributes,
                documentation: field
                    .documentation
                    .as_ref()
                    .map(|text| ast::Comment { text: text.to_owned() }),
                span: ast::Span::empty(),
                is_commented_out: field.is_commented_out,
            });
        }

        ast::CompositeType {
            name: ast::Identifier::new(&r#type.name),
            fields,
            documentation: None,
            span: ast::Span::empty(),
        }
    }

    pub fn field_array(fields: &[String]) -> Vec<ast::Expression> {
        fields
            .iter()
            .map(|f| ast::Expression::ConstantValue(f.to_string(), ast::Span::empty()))
            .collect()
    }

    pub fn pk_field_array(fields: &[PrimaryKeyField]) -> Vec<ast::Expression> {
        fields
            .iter()
            .map(|f| {
                let mut args = vec![];
                args.extend(f.length.map(|length| ast::Argument::new_numeric("length", length)));

                args.extend(
                    (f.sort_order == Some(SortOrder::Desc)).then(|| ast::Argument::new_constant("sort", "Desc")),
                );

                if args.is_empty() {
                    ast::Expression::ConstantValue(f.name.clone(), ast::Span::empty())
                } else {
                    ast::Expression::Function(
                        f.name.clone(),
                        ast::ArgumentsList {
                            arguments: args,
                            ..Default::default()
                        },
                        ast::Span::empty(),
                    )
                }
            })
            .collect()
    }

    pub fn index_field_array(fields: &[IndexField], always_render_sort_order: bool) -> Vec<ast::Expression> {
        fields
            .iter()
            .map(|f| {
                let mut args = vec![];

                args.extend(f.length.map(|length| ast::Argument::new_numeric("length", length)));

                if always_render_sort_order {
                    let ordering = f.sort_order.map(|ordering| match ordering {
                        SortOrder::Asc => ast::Argument::new_constant("sort", "Asc"),
                        SortOrder::Desc => ast::Argument::new_constant("sort", "Desc"),
                    });

                    args.extend(ordering);
                } else {
                    let ordering = f
                        .sort_order
                        .filter(|s| *s == SortOrder::Desc)
                        .map(|_| ast::Argument::new_constant("sort", "Desc"));

                    args.extend(ordering);
                }

                if args.is_empty() {
                    ast::Expression::ConstantValue(f.name.clone(), ast::Span::empty())
                } else {
                    ast::Expression::Function(
                        f.name.clone(),
                        ast::ArgumentsList {
                            arguments: args,
                            ..Default::default()
                        },
                        ast::Span::empty(),
                    )
                }
            })
            .collect()
    }
}

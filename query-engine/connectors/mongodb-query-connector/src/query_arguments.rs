use crate::{filter::convert_filter, join::JoinStage, BsonTransform, IntoBson};
use connector_interface::{AggregationSelection, Filter, QueryArguments};
use mongodb::{
    bson::{doc, Bson, Document},
    options::{AggregateOptions, FindOptions},
    Collection, Cursor,
};
use prisma_models::{ModelProjection, OrderBy, ScalarFieldRef, SortOrder};

/// Translated query arguments ready to use in mongo find or aggregation queries.
#[derive(Debug, Default)]
pub(crate) struct MongoQueryArgs {
    /// Pre-join, "normal" filters.
    pub(crate) query: Option<Document>,

    /// Join stages.
    pub(crate) joins: Vec<JoinStage>,

    /// Filters that can only be applied after the joins
    /// or aggregations added the required data to execute them.
    pub(crate) join_filters: Vec<Document>,

    /// Aggregation stages.
    pub(crate) aggregations: Vec<Document>,

    /// Filters that can only be applied after the aggregations
    /// transformed the documents.
    pub(crate) aggregation_filters: Vec<Document>,

    /// Order by builder.
    pub(crate) order_builder: Option<OrderByBuilder>,

    /// Finalized ordering.
    pub(crate) order: Option<Document>,

    /// Skip a number of documents at the start of the result.
    pub(crate) skip: Option<i64>,

    /// Take only a certain number of documents from the result.
    pub(crate) limit: Option<i64>,

    /// Projection document to scope down return fields.
    pub(crate) projection: Option<Document>,

    /// Switch to indicate the underlying aggregation is a `group_by` query.
    /// This is due to legacy drift in how `aggregate` and `group_by` work in
    /// the API and will hopefully be merged again in the future.
    pub(crate) is_group_by_query: bool,
}

impl MongoQueryArgs {
    /// Turns the query args into a find operation on the collection.
    /// Depending on the arguments, either an aggregation pipeline or a plain query is build and run.
    pub(crate) async fn find_documents(mut self, coll: Collection) -> crate::Result<Cursor> {
        self.finalize();

        if self.joins.is_empty() && self.aggregations.is_empty() {
            self.execute_find_query(coll).await
        } else {
            self.execute_pipeline_query(coll).await
        }
    }

    async fn execute_find_query(self, coll: Collection) -> crate::Result<Cursor> {
        let find_options = FindOptions::builder()
            .projection(self.projection)
            .limit(self.limit)
            .skip(self.skip)
            .sort(self.order)
            .build();

        Ok(coll.find(self.query, find_options).await?)
    }

    async fn execute_pipeline_query(self, coll: Collection) -> crate::Result<Cursor> {
        let opts = AggregateOptions::builder().allow_disk_use(true).build();
        let mut stages = vec![];

        // Initial $matches
        if let Some(query) = self.query {
            stages.push(doc! { "$match": query })
        };

        // Joins ($lookup)
        stages.extend(self.joins.into_iter().map(|stage| stage.build()));

        // Post-join $matches
        stages.extend(self.join_filters.into_iter().map(|filter| doc! { "$match": filter }));

        // If the query is a group by, skip, take, sort all apply to the _groups_, not the documents
        // before. If it is a plain aggregation, then the aggregate stages need to be _after_ these, because
        // they apply to the documents to be aggregated, not the aggregations (legacy meh).

        if self.is_group_by_query {
            // Aggregates
            stages.extend(self.aggregations.clone());

            // Aggregation filters
            stages.extend(
                self.aggregation_filters
                    .clone()
                    .into_iter()
                    .map(|filter| doc! { "$match": filter }),
            );
        }

        // $sort
        if let Some(order) = self.order {
            stages.push(doc! { "$sort": order })
        };

        // $skip
        if let Some(skip) = self.skip {
            stages.push(doc! { "$skip": skip });
        };

        // $limit
        if let Some(limit) = self.limit {
            stages.push(doc! { "$limit": limit });
        };

        // $project
        if let Some(projection) = self.projection {
            stages.push(doc! { "$project": projection });
        };

        if !self.is_group_by_query {
            // Aggregates
            stages.extend(self.aggregations);

            // Aggregation filters
            stages.extend(
                self.aggregation_filters
                    .into_iter()
                    .map(|filter| doc! { "$match": filter }),
            );
        }

        dbg!(&stages);

        Ok(coll.aggregate(stages, opts).await?)
    }

    pub(crate) fn new(args: QueryArguments) -> crate::Result<MongoQueryArgs> {
        let reverse_order = args.take.map(|t| t < 0).unwrap_or(false);
        let order_builder = Some(OrderByBuilder::new(args.order_by, reverse_order));
        let mut post_filters = vec![];
        let mut joins = vec![];

        let query = match args.filter {
            Some(filter) => {
                // If a filter comes with joins, it needs to be run _after_ the initial filter query / $matches.
                let (filter, filter_joins) = convert_filter(filter, false)?.render();
                if !filter_joins.is_empty() {
                    joins.extend(filter_joins);
                    post_filters.push(filter);

                    None
                } else {
                    Some(filter)
                }
            }
            None => None,
        };

        Ok(MongoQueryArgs {
            query,
            join_filters: post_filters,
            joins,
            order_builder,
            skip: skip(args.skip, args.ignore_skip),
            limit: take(args.take, args.ignore_take),
            ..Default::default()
        })
    }

    /// Adds a final projection onto the fields specified by the `ModelProjection`.
    pub fn with_model_projection(mut self, selected_fields: ModelProjection) -> crate::Result<Self> {
        let projection = selected_fields.into_bson()?.into_document()?;
        self.projection = Some(projection);

        Ok(self)
    }

    /// Adds group-by fields with their aggregations to this query.
    pub fn with_groupings(mut self, by_fields: Vec<ScalarFieldRef>, aggregations: &[AggregationSelection]) -> Self {
        let grouping = if by_fields.is_empty() {
            Bson::Null // Null => group over the entire collection.
        } else {
            let mut group_doc = Document::new();
            self.is_group_by_query = true;

            for field in by_fields {
                group_doc.insert(field.db_name(), format!("${}", field.db_name()));
            }

            group_doc.into()
        };

        let mut grouping_stage = doc! { "_id": grouping };

        for selection in aggregations {
            match selection {
                AggregationSelection::Field(_) => (),
                AggregationSelection::Count { all, fields } => {
                    if *all {
                        grouping_stage.insert("count_all", doc! { "$sum": 1 });
                    }

                    let pairs = aggregation_pairs("count", fields);
                    grouping_stage.extend(pairs);
                }
                AggregationSelection::Average(fields) => {
                    let pairs = aggregation_pairs("avg", fields);
                    grouping_stage.extend(pairs);
                }
                AggregationSelection::Sum(fields) => {
                    let pairs = aggregation_pairs("sum", fields);
                    grouping_stage.extend(pairs);
                }
                AggregationSelection::Min(fields) => {
                    let pairs = aggregation_pairs("min", fields);
                    grouping_stage.extend(pairs);
                }
                AggregationSelection::Max(fields) => {
                    let pairs = aggregation_pairs("max", fields);
                    grouping_stage.extend(pairs);
                }
            }
        }

        self.aggregations.push(doc! { "$group": grouping_stage });
        self
    }

    /// Adds aggregation filters based on a having scalar filter.
    pub fn with_having(mut self, having: Option<Filter>) -> crate::Result<Self> {
        if let Some(filter) = having {
            let (filter_doc, _) = convert_filter(filter, false)?.render();
            self.aggregation_filters.push(filter_doc);
        }

        Ok(self)
    }

    fn finalize(&mut self) {
        let builder = self.order_builder.take().expect("Mongo args already finalized.");
        let (order, joins) = builder.build(self.is_group_by_query);

        self.joins.extend(joins);
        self.order = order;
    }
}

/// Builder for `sort` mongo documents.
/// Building of orderBy needs to be deferred until all other args are complete
/// to have all information necessary to build the correct sort arguments.
#[derive(Debug, Default)]
pub(crate) struct OrderByBuilder {
    order_bys: Vec<OrderBy>,
    reverse: bool,
}

impl OrderByBuilder {
    pub fn new(order_bys: Vec<OrderBy>, reverse: bool) -> Self {
        Self { order_bys, reverse }
    }

    /// Builds and renders a Mongo sort document.
    /// `is_group_by` signals that the ordering is for a grouping,
    /// requiring a prefix to refer to the correct document nesting.
    fn build(self, is_group_by: bool) -> (Option<Document>, Vec<JoinStage>) {
        if self.order_bys.is_empty() {
            return (None, vec![]);
        }

        let mut order_doc = Document::new();

        for order_by in self.order_bys {
            let field = if is_group_by {
                // Explanation: All group by fields go into the _id key of the result document.
                // As it is the only point where the flat scalars are contained for the group,
                // we beed to refer to the object
                format!("_id.{}", order_by.field.db_name())
            } else {
                order_by.field.db_name().to_owned()
            };

            // Mongo: -1 -> DESC, 1 -> ASC
            match (order_by.sort_order, self.reverse) {
                (SortOrder::Ascending, true) => order_doc.insert(field, -1),
                (SortOrder::Descending, true) => order_doc.insert(field, 1),
                (SortOrder::Ascending, false) => order_doc.insert(field, 1),
                (SortOrder::Descending, false) => order_doc.insert(field, -1),
            };
        }

        // todo joins
        (Some(order_doc), vec![])
    }
}

/// Utilities below ///

fn aggregation_pairs(op: &str, fields: &[ScalarFieldRef]) -> Vec<(String, Bson)> {
    fields
        .into_iter()
        .map(|field| {
            (
                format!("{}_{}", op, field.db_name()),
                doc! { format!("${}", op): format!("${}", field.db_name()) }.into(),
            )
        })
        .collect()
}

fn skip(skip: Option<i64>, ignore: bool) -> Option<i64> {
    if ignore {
        None
    } else {
        skip
    }
}

fn take(take: Option<i64>, ignore: bool) -> Option<i64> {
    if ignore {
        None
    } else {
        take.map(|t| if t < 0 { -t } else { t })
    }
}

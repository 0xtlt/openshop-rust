//! `#[derive(Model)]`: a struct as the single description of a table, with
//! typed rows, and migrations generated from what changed.

mod common;

use chrono::{DateTime, Utc};
use common::{TempProject, empty_database};
use openshop::db::migrate::{FRAMEWORK_MIGRATIONS, check_migrations, generate_migrations, migrate_schema};
use openshop::testing::TestDb;
use openshop::{Model, OpenShopConfig, PgPool, Record, Value, json};
use uuid::Uuid;

const SHOP: &str = "one.myshopify.com";
const OTHER_SHOP: &str = "two.myshopify.com";

#[derive(Debug, Clone, PartialEq, Model)]
#[model(
    table = "reviews",
    index(shop, product_id),
    unique(shop, product_id, author, name = "reviews_one_per_author")
)]
struct Review {
    id: Uuid,
    shop: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    product_id: String,
    author: String,
    rating: i32,
    note: Option<String>,
    #[model(default = "false")]
    approved: bool,
    #[model(default = "'{}'::jsonb")]
    meta: Value,
    tags: Vec<String>,
}

/// A table without shop or timestamps, with a renamed column and a keyword field.
#[derive(Debug, Clone, PartialEq, Model)]
#[model(table = "settings")]
struct Setting {
    id: Uuid,
    #[model(unique)]
    key: String,
    #[model(column = "val", index)]
    value: i64,
    #[model(sql_type = "varchar(16)")]
    r#type: String,
}

fn new_review(product: &str, author: &str, rating: i32) -> NewReview {
    NewReview {
        product_id: product.to_string(),
        author: author.to_string(),
        rating,
        note: None,
        approved: None,
        meta: None,
        tags: Vec::new(),
    }
}

async fn database() -> TestDb {
    TestDb::create_with_models(&[Review::model(), Setting::model()])
        .await
        .unwrap()
}

#[test]
fn the_struct_describes_the_table() {
    assert_eq!((Review::TABLE, Review::HAS_SHOP), ("reviews", true));
    assert_eq!(
        Review::COLUMNS,
        [
            "id",
            "shop",
            "created_at",
            "updated_at",
            "product_id",
            "author",
            "rating",
            "note",
            "approved",
            "meta",
            "tags"
        ]
    );
    assert_eq!(
        Review::model().create_sql(),
        "CREATE TABLE IF NOT EXISTS \"reviews\" (\n\
         \x20 \"id\" uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,\n\
         \x20 \"shop\" text NOT NULL,\n\
         \x20 \"created_at\" timestamp with time zone DEFAULT now() NOT NULL,\n\
         \x20 \"updated_at\" timestamp with time zone DEFAULT now() NOT NULL,\n\
         \x20 \"product_id\" text NOT NULL,\n\
         \x20 \"author\" text NOT NULL,\n\
         \x20 \"rating\" integer NOT NULL,\n\
         \x20 \"note\" text,\n\
         \x20 \"approved\" boolean DEFAULT false NOT NULL,\n\
         \x20 \"meta\" jsonb DEFAULT '{}'::jsonb NOT NULL,\n\
         \x20 \"tags\" text[] NOT NULL\n\
         );\n\
         CREATE INDEX IF NOT EXISTS \"reviews_shop_product_id_idx\" ON \"reviews\" (\"shop\", \"product_id\");\n\
         CREATE UNIQUE INDEX IF NOT EXISTS \"reviews_one_per_author\" ON \"reviews\" (\"shop\", \"product_id\", \"author\");"
    );

    assert_eq!((Setting::TABLE, Setting::HAS_SHOP), ("settings", false));
    assert_eq!(
        Setting::model().create_sql(),
        "CREATE TABLE IF NOT EXISTS \"settings\" (\n\
         \x20 \"id\" uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,\n\
         \x20 \"key\" text NOT NULL,\n\
         \x20 \"val\" bigint NOT NULL,\n\
         \x20 \"type\" character varying(16) NOT NULL\n\
         );\n\
         CREATE UNIQUE INDEX IF NOT EXISTS \"settings_key_unique\" ON \"settings\" (\"key\");\n\
         CREATE INDEX IF NOT EXISTS \"settings_val_idx\" ON \"settings\" (\"val\");"
    );
    assert_eq!(
        (Review::RATING.name(), Setting::VALUE.name(), Setting::TYPE.name()),
        ("rating", "val", "type")
    );

    // A registered model is what the configuration pushes and migrates.
    let config = OpenShopConfig::builder().model_of::<Review>().build().unwrap();
    assert_eq!(config.models, [Review::model()]);
}

#[tokio::test]
async fn inserting_fills_the_shop_and_the_defaults() {
    let db = database().await;
    let reviews = Review::for_shop(&db.pool, SHOP);

    let review = reviews.insert(new_review("p1", "ada", 5)).await.unwrap();
    assert_eq!(review.shop, SHOP);
    assert_eq!(
        (review.product_id.as_str(), review.author.as_str(), review.rating),
        ("p1", "ada", 5)
    );
    assert_eq!(review.note, None);
    // Left to `None`: the defaults of the columns.
    assert!(!review.approved);
    assert_eq!(review.meta, json!({}));
    assert_eq!(review.created_at, review.updated_at);
    assert_eq!(review.id(), review.id);

    let detailed = reviews
        .insert(NewReview {
            note: Some("Great".to_string()),
            approved: Some(true),
            meta: Some(json!({ "source": "email" })),
            tags: vec!["verified".to_string()],
            ..new_review("p2", "ada", 4)
        })
        .await
        .unwrap();
    assert_eq!(detailed.note.as_deref(), Some("Great"));
    assert!(detailed.approved);
    assert_eq!(detailed.meta, json!({ "source": "email" }));
    assert_eq!(detailed.tags, ["verified"]);

    assert_eq!(reviews.find(review.id).await.unwrap(), Some(review));
    assert_eq!(reviews.count().await.unwrap(), 2);

    // The unique index of the model is enforced.
    let error = reviews.insert(new_review("p1", "ada", 1)).await.unwrap_err();
    assert!(error.to_string().contains("reviews_one_per_author"), "{error}");
    db.cleanup().await.unwrap();
}

#[tokio::test]
async fn the_rows_of_a_shop_never_reach_another_shop() {
    let db = database().await;
    let mine = Review::for_shop(&db.pool, SHOP);
    let theirs = Review::for_shop(&db.pool, OTHER_SHOP);
    let review = mine.insert(new_review("p1", "ada", 5)).await.unwrap();
    let foreign = theirs.insert(new_review("p1", "bob", 1)).await.unwrap();

    // By id, by listing, by counting.
    assert_eq!(mine.find(foreign.id).await.unwrap(), None);
    assert_eq!(mine.all().await.unwrap(), std::slice::from_ref(&review));
    assert_eq!(mine.count().await.unwrap(), 1);
    assert_eq!(mine.filter(Review::PRODUCT_ID.eq("p1")).count().await.unwrap(), 1);

    // An update or a deletion of a foreign row does nothing.
    let mut hijacked = foreign.clone();
    hijacked.rating = 5;
    assert_eq!(mine.save(&hijacked).await.unwrap(), None);
    assert!(!mine.delete(foreign.id).await.unwrap());
    assert_eq!(mine.filter(Review::RATING.lte(5)).delete().await.unwrap(), 1);
    assert_eq!(theirs.find(foreign.id).await.unwrap(), Some(foreign.clone()));

    // Every shop at once is explicit, and cannot insert.
    let everywhere = Review::all_shops(&db.pool);
    assert_eq!(everywhere.all().await.unwrap(), [foreign]);
    let error = everywhere.insert(new_review("p9", "eve", 3)).await.unwrap_err();
    assert_eq!(
        error.to_string(),
        "[openshop] A row of \"reviews\" belongs to a shop: insert it through `for_shop`"
    );
    db.cleanup().await.unwrap();
}

#[tokio::test]
async fn queries_filter_sort_and_limit_with_typed_columns() {
    let db = database().await;
    let reviews = Review::for_shop(&db.pool, SHOP);
    for (product, author, rating, note) in [
        ("p1", "ada", 5, Some("Great")),
        ("p1", "bob", 2, None),
        ("p2", "ada", 4, None),
        ("p2", "cyd", 3, Some("Fine")),
    ] {
        let new = NewReview {
            note: note.map(str::to_string),
            ..new_review(product, author, rating)
        };
        reviews.insert(new).await.unwrap();
    }
    let authors = |rows: Vec<Review>| -> Vec<String> {
        rows.into_iter()
            .map(|row| format!("{}:{}", row.product_id, row.author))
            .collect()
    };

    let best = reviews
        .filter(Review::RATING.gte(4))
        .order_by(Review::RATING.desc())
        .all()
        .await
        .unwrap();
    assert_eq!(authors(best), ["p1:ada", "p2:ada"]);

    let of_product = reviews
        .filter(Review::PRODUCT_ID.eq("p1"))
        .filter(Review::RATING.lt(5))
        .all()
        .await
        .unwrap();
    assert_eq!(authors(of_product), ["p1:bob"]);

    let ordered = reviews
        .order_by(Review::PRODUCT_ID.desc())
        .order_by(Review::AUTHOR.asc());
    assert_eq!(
        authors(ordered.all().await.unwrap()),
        ["p2:ada", "p2:cyd", "p1:ada", "p1:bob"]
    );

    let page = reviews
        .order_by(Review::RATING.asc())
        .limit(2)
        .offset(1)
        .all()
        .await
        .unwrap();
    assert_eq!(authors(page), ["p2:cyd", "p2:ada"]);

    let commented = reviews.filter(Review::NOTE.is_not_null()).count().await.unwrap();
    assert_eq!(commented, 2);
    let silent = reviews
        .filter(Review::NOTE.is_null())
        .filter(Review::AUTHOR.ne("bob"))
        .first()
        .await
        .unwrap();
    assert_eq!(silent.map(|row| row.author).as_deref(), Some("ada"));
    // A nullable column is compared with the value it holds.
    assert_eq!(reviews.filter(Review::NOTE.eq("Fine")).count().await.unwrap(), 1);

    let some = reviews
        .filter(Review::AUTHOR.is_in(["bob", "cyd", "zed"]))
        .order_by(Review::AUTHOR.asc())
        .all()
        .await
        .unwrap();
    assert_eq!(authors(some), ["p1:bob", "p2:cyd"]);
    assert_eq!(
        reviews
            .filter(Review::AUTHOR.is_in(Vec::<String>::new()))
            .count()
            .await
            .unwrap(),
        0
    );

    let lowest = reviews.order_by(Review::RATING.asc()).first().await.unwrap();
    assert_eq!(lowest.map(|row| row.rating), Some(2));
    assert!(reviews.first().await.unwrap().is_some());

    let error = reviews.limit(1).delete().await.unwrap_err();
    assert_eq!(
        error.to_string(),
        "[openshop] Deleting rows of \"reviews\" does not take a limit or an offset: filter the rows to delete"
    );
    assert_eq!(reviews.filter(Review::RATING.lt(4)).delete().await.unwrap(), 2);
    assert_eq!(reviews.count().await.unwrap(), 2);
    db.cleanup().await.unwrap();
}

#[tokio::test]
async fn saving_writes_the_fields_and_touches_updated_at() {
    let db = database().await;
    let reviews = Review::for_shop(&db.pool, SHOP);
    let mut review = reviews.insert(new_review("p1", "ada", 3)).await.unwrap();
    sqlx::query("UPDATE reviews SET created_at = now() - interval '1 hour', updated_at = now() - interval '1 hour'")
        .execute(&db.pool)
        .await
        .unwrap();
    let before = reviews.find(review.id).await.unwrap().unwrap();

    review.rating = 5;
    review.note = Some("Changed my mind".to_string());
    review.approved = true;
    review.tags.push("edited".to_string());
    // What the database owns is not written back.
    review.shop = OTHER_SHOP.to_string();
    review.created_at = Utc::now();

    let saved = reviews.save(&review).await.unwrap().expect("the row exists");
    assert_eq!(
        (saved.rating, saved.note.as_deref(), saved.approved),
        (5, Some("Changed my mind"), true)
    );
    assert_eq!(saved.tags, ["edited"]);
    assert_eq!(saved.shop, SHOP);
    assert_eq!(saved.created_at, before.created_at);
    assert!(saved.updated_at > before.updated_at);
    assert_eq!(reviews.find(review.id).await.unwrap(), Some(saved));

    assert!(reviews.delete(review.id).await.unwrap());
    assert_eq!(reviews.save(&review).await.unwrap(), None);
    assert!(!reviews.delete(review.id).await.unwrap());
    db.cleanup().await.unwrap();
}

#[tokio::test]
async fn a_model_without_shop_has_plain_rows() {
    let db = database().await;
    let settings = Setting::rows(&db.pool);

    let mut limit = settings
        .insert(NewSetting {
            key: "limit".to_string(),
            value: 10,
            r#type: "number".to_string(),
        })
        .await
        .unwrap();
    assert_eq!(
        (limit.key.as_str(), limit.value, limit.r#type.as_str()),
        ("limit", 10, "number")
    );

    // The field `value` is the column `val`.
    let stored: i64 = sqlx::query_scalar("SELECT val FROM settings WHERE key = 'limit'")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(stored, 10);
    assert_eq!(settings.filter(Setting::VALUE.gt(5)).count().await.unwrap(), 1);
    assert_eq!(settings.filter(Setting::TYPE.eq("number")).count().await.unwrap(), 1);

    limit.value = 20;
    assert_eq!(settings.save(&limit).await.unwrap(), Some(limit.clone()));

    let duplicate = NewSetting {
        key: "limit".to_string(),
        value: 1,
        r#type: "number".to_string(),
    };
    let error = settings.insert(duplicate).await.unwrap_err();
    assert!(error.to_string().contains("settings_key_unique"), "{error}");
    assert!(settings.delete(limit.id).await.unwrap());
    db.cleanup().await.unwrap();
}

// ─── Migrations ──────────────────────────────────────────────────────

/// The same table one release later: a column renamed and retyped, one
/// added with a default, one added nullable, one dropped, an index replaced.
#[derive(Debug, Clone, PartialEq, Model)]
#[model(table = "reviews", index(shop, score))]
struct ReviewV2 {
    id: Uuid,
    shop: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    product_id: String,
    author: String,
    #[model(renamed_from = "rating")]
    score: i64,
    note: Option<String>,
    #[model(default = "false")]
    approved: bool,
    #[model(default = "'{}'::jsonb")]
    meta: Value,
    #[model(default = "'en'")]
    locale: String,
    reply: Option<String>,
}

async fn columns(pool: &PgPool, table: &str) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT column_name::text || ' ' || data_type::text FROM information_schema.columns \
         WHERE table_schema = current_schema() AND table_name = $1 ORDER BY ordinal_position",
    )
    .bind(table)
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn indexes(pool: &PgPool, table: &str) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT indexname::text FROM pg_indexes WHERE schemaname = current_schema() AND tablename = $1 ORDER BY 1",
    )
    .bind(table)
    .fetch_all(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn generated_migrations_follow_the_models_from_one_release_to_the_next() {
    let db = empty_database().await;
    let project = TempProject::new();

    // First release: the framework schema and the table of the model.
    let first = generate_migrations(project.path(), &[], false, &[Review::model()]).unwrap();
    assert_eq!(first.files.len(), FRAMEWORK_MIGRATIONS.len() + 1);
    assert!(first.warnings.is_empty());
    migrate_schema(&db.pool, project.path()).await.unwrap();
    check_migrations(project.path(), &[Review::model()]).unwrap();

    let reviews = Review::for_shop(&db.pool, SHOP);
    let review = reviews.insert(new_review("p1", "ada", 5)).await.unwrap();
    assert_eq!(
        indexes(&db.pool, "reviews").await,
        ["reviews_one_per_author", "reviews_pkey", "reviews_shop_product_id_idx"]
    );

    // Second release: the struct changed, `check` refuses to go on without a migration.
    let error = check_migrations(project.path(), &[ReviewV2::model()]).unwrap_err();
    assert!(
        error
            .to_string()
            .starts_with("[openshop] The models changed since the last generated migration:"),
        "{error}"
    );

    let second = generate_migrations(
        project.path(),
        &["--name=reviews_v2".to_string()],
        false,
        &[ReviewV2::model()],
    )
    .unwrap();
    assert_eq!(second.files.len(), 1);
    assert_eq!(
        std::fs::read_to_string(&second.files[0]).unwrap(),
        "-- reviews_v2\n\
         -- Generated by `openshop migrate generate`. Review before applying.\n\
         \n\
         ALTER TABLE \"reviews\" RENAME COLUMN \"rating\" TO \"score\";\n\
         \n\
         ALTER TABLE \"reviews\" ADD COLUMN IF NOT EXISTS \"locale\" text DEFAULT 'en' NOT NULL;\n\
         \n\
         ALTER TABLE \"reviews\" ADD COLUMN IF NOT EXISTS \"reply\" text;\n\
         \n\
         -- WARNING: converts the values of reviews.score from integer to bigint. Check that the cast is the one you want.\n\
         ALTER TABLE \"reviews\" ALTER COLUMN \"score\" TYPE bigint USING \"score\"::bigint;\n\
         \n\
         DROP INDEX IF EXISTS \"reviews_shop_product_id_idx\";\n\
         \n\
         DROP INDEX IF EXISTS \"reviews_one_per_author\";\n\
         \n\
         -- WARNING: drops reviews.tags and its data.\n\
         ALTER TABLE \"reviews\" DROP COLUMN IF EXISTS \"tags\";\n\
         \n\
         CREATE INDEX IF NOT EXISTS \"reviews_shop_score_idx\" ON \"reviews\" (\"shop\", \"score\");\n"
    );
    assert_eq!(
        second.warnings,
        [
            "converts the values of reviews.score from integer to bigint. Check that the cast is the one you want.",
            "drops reviews.tags and its data.",
        ]
    );

    let applied = migrate_schema(&db.pool, project.path()).await.unwrap();
    assert_eq!(applied.len(), 1);
    assert!(applied[0].ends_with("_reviews_v2"));
    check_migrations(project.path(), &[ReviewV2::model()]).unwrap();

    // The table is the one the new struct describes, with its data.
    assert_eq!(
        columns(&db.pool, "reviews").await,
        [
            "id uuid",
            "shop text",
            "created_at timestamp with time zone",
            "updated_at timestamp with time zone",
            "product_id text",
            "author text",
            "score bigint",
            "note text",
            "approved boolean",
            "meta jsonb",
            "locale text",
            "reply text",
        ]
    );
    assert_eq!(
        indexes(&db.pool, "reviews").await,
        ["reviews_pkey", "reviews_shop_score_idx"]
    );
    let migrated = ReviewV2::for_shop(&db.pool, SHOP)
        .find(review.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (migrated.score, migrated.locale.as_str(), migrated.reply),
        (5, "en", None)
    );

    // Nothing is left to generate.
    let third = generate_migrations(project.path(), &[], false, &[ReviewV2::model()]).unwrap();
    assert_eq!((third.files.len(), third.snapshot), (0, None));
    db.cleanup().await.unwrap();
}

#[tokio::test]
async fn the_development_push_adds_the_columns_a_model_gained() {
    let db = TestDb::create_with_models(&[Review::model()]).await.unwrap();
    let review = Review::for_shop(&db.pool, SHOP)
        .insert(new_review("p1", "ada", 5))
        .await
        .unwrap();

    // The next start of `dev`, with the struct of the next release.
    openshop::db::migrate::push_schema(&db.pool, &[ReviewV2::model()], false)
        .await
        .unwrap();

    let names: Vec<String> = columns(&db.pool, "reviews").await;
    assert!(names.contains(&"locale text".to_string()), "{names:?}");
    assert!(names.contains(&"reply text".to_string()), "{names:?}");
    // A column that needs a value for existing rows is added too when it has a default; `score`
    // has none, so it is left to a migration, and nothing is renamed or dropped.
    assert!(names.contains(&"rating integer".to_string()), "{names:?}");
    assert!(names.contains(&"tags ARRAY".to_string()), "{names:?}");
    assert!(!names.iter().any(|name| name.starts_with("score ")), "{names:?}");
    let locale: String = sqlx::query_scalar("SELECT locale FROM reviews WHERE id = $1")
        .bind(review.id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(locale, "en");
    db.cleanup().await.unwrap();
}

// ─── Foreign keys ────────────────────────────────────────────────────

/// A vote on a review. Deleting the review deletes its votes; a vote can
/// answer another vote, and survives it.
#[derive(Debug, Clone, PartialEq, Model)]
#[model(table = "review_votes", index(review_id))]
struct ReviewVote {
    id: Uuid,
    shop: String,
    #[model(references(Review), on_delete = "cascade")]
    review_id: Uuid,
    value: i32,
    #[model(references(ReviewVote), on_delete = "set null")]
    answers: Option<Uuid>,
}

fn vote(review: &Review, value: i32) -> NewReviewVote {
    NewReviewVote {
        review_id: review.id,
        value,
        answers: None,
    }
}

async fn foreign_keys(pool: &PgPool, table: &str) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT conname::text || ': ' || pg_get_constraintdef(oid) FROM pg_constraint \
         WHERE contype = 'f' AND conrelid = to_regclass($1) ORDER BY conname",
    )
    .bind(format!("\"{table}\""))
    .fetch_all(pool)
    .await
    .unwrap()
}

#[test]
fn references_are_part_of_the_model() {
    let model = ReviewVote::model();
    // The table itself is created without them, so that models register in any order.
    assert!(!model.create_sql().contains("REFERENCES"));
    let statements: Vec<String> = model
        .foreign_keys()
        .iter()
        .map(|foreign_key| foreign_key.add_sql(&model.table))
        .collect();
    assert_eq!(
        statements,
        [
            "ALTER TABLE \"review_votes\" ADD CONSTRAINT \"review_votes_review_id_fkey\" \
             FOREIGN KEY (\"review_id\") REFERENCES \"reviews\" (\"id\") ON DELETE CASCADE;",
            "ALTER TABLE \"review_votes\" ADD CONSTRAINT \"review_votes_answers_fkey\" \
             FOREIGN KEY (\"answers\") REFERENCES \"review_votes\" (\"id\") ON DELETE SET NULL;",
        ]
    );
}

#[tokio::test]
async fn the_database_enforces_references() {
    // The referencing model comes first: the push still creates both tables, then the constraints.
    let db = TestDb::create_with_models(&[ReviewVote::model(), Review::model()])
        .await
        .unwrap();
    assert_eq!(
        foreign_keys(&db.pool, "review_votes").await,
        [
            "review_votes_answers_fkey: FOREIGN KEY (answers) REFERENCES review_votes(id) ON DELETE SET NULL",
            "review_votes_review_id_fkey: FOREIGN KEY (review_id) REFERENCES reviews(id) ON DELETE CASCADE",
        ]
    );
    // Pushing again, as at the next start of `dev`, changes nothing.
    openshop::db::migrate::push_schema(&db.pool, &[ReviewVote::model(), Review::model()], false)
        .await
        .unwrap();
    assert_eq!(foreign_keys(&db.pool, "review_votes").await.len(), 2);

    let reviews = Review::for_shop(&db.pool, SHOP);
    let votes = ReviewVote::for_shop(&db.pool, SHOP);
    let kept = reviews.insert(new_review("p1", "ada", 5)).await.unwrap();
    let removed = reviews.insert(new_review("p2", "ada", 1)).await.unwrap();

    // A vote needs an existing review.
    let orphan = NewReviewVote {
        review_id: Uuid::new_v4(),
        value: 1,
        answers: None,
    };
    let error = votes.insert(orphan).await.unwrap_err();
    assert!(error.to_string().contains("review_votes_review_id_fkey"), "{error}");

    let first = votes.insert(vote(&kept, 1)).await.unwrap();
    let answer = votes
        .insert(NewReviewVote {
            answers: Some(first.id),
            ..vote(&kept, -1)
        })
        .await
        .unwrap();
    votes.insert(vote(&removed, 1)).await.unwrap();
    assert_eq!(
        votes.filter(ReviewVote::REVIEW_ID.eq(kept.id)).count().await.unwrap(),
        2
    );

    // `cascade`: the votes of a deleted review go with it.
    assert!(reviews.delete(removed.id).await.unwrap());
    assert_eq!(votes.count().await.unwrap(), 2);
    // `set null`: an answer survives the vote it answered.
    assert!(votes.delete(first.id).await.unwrap());
    assert_eq!(votes.find(answer.id).await.unwrap().unwrap().answers, None);
    db.cleanup().await.unwrap();
}

/// The votes one release later: the link is kept under another column name,
/// and answers are no longer tracked.
#[derive(Debug, Clone, PartialEq, Model)]
#[model(table = "review_votes")]
struct ReviewVoteV2 {
    id: Uuid,
    shop: String,
    #[model(references(Review), on_delete = "cascade", renamed_from = "review_id")]
    target_id: Uuid,
    value: i32,
}

#[tokio::test]
async fn generated_migrations_carry_the_references() {
    let db = empty_database().await;
    let project = TempProject::new();

    let first = generate_migrations(project.path(), &[], false, &[ReviewVote::model(), Review::model()]).unwrap();
    assert!(first.warnings.is_empty());
    let sql = std::fs::read_to_string(first.files.last().unwrap()).unwrap();
    let position = |needle: &str| sql.find(needle).unwrap_or_else(|| panic!("{needle} in:\n{sql}"));
    let constraint = "ALTER TABLE \"review_votes\" ADD CONSTRAINT \"review_votes_review_id_fkey\"";
    assert!(position("CREATE TABLE IF NOT EXISTS \"reviews\"") < position(constraint));
    assert!(position("CREATE TABLE IF NOT EXISTS \"review_votes\"") < position(constraint));
    migrate_schema(&db.pool, project.path()).await.unwrap();
    assert_eq!(foreign_keys(&db.pool, "review_votes").await.len(), 2);

    let review = Review::for_shop(&db.pool, SHOP)
        .insert(new_review("p1", "ada", 5))
        .await
        .unwrap();
    let vote = ReviewVote::for_shop(&db.pool, SHOP)
        .insert(vote(&review, 1))
        .await
        .unwrap();

    let second = generate_migrations(project.path(), &[], false, &[ReviewVoteV2::model(), Review::model()]).unwrap();
    assert_eq!(
        std::fs::read_to_string(&second.files[0]).unwrap(),
        "-- models\n\
         -- Generated by `openshop migrate generate`. Review before applying.\n\
         \n\
         ALTER TABLE \"review_votes\" RENAME COLUMN \"review_id\" TO \"target_id\";\n\
         \n\
         ALTER TABLE \"review_votes\" RENAME CONSTRAINT \"review_votes_review_id_fkey\" TO \"review_votes_target_id_fkey\";\n\
         \n\
         ALTER TABLE \"review_votes\" DROP CONSTRAINT IF EXISTS \"review_votes_answers_fkey\";\n\
         \n\
         DROP INDEX IF EXISTS \"review_votes_review_id_idx\";\n\
         \n\
         -- WARNING: drops review_votes.answers and its data.\n\
         ALTER TABLE \"review_votes\" DROP COLUMN IF EXISTS \"answers\";\n"
    );
    migrate_schema(&db.pool, project.path()).await.unwrap();
    check_migrations(project.path(), &[ReviewVoteV2::model(), Review::model()]).unwrap();

    assert_eq!(
        foreign_keys(&db.pool, "review_votes").await,
        ["review_votes_target_id_fkey: FOREIGN KEY (target_id) REFERENCES reviews(id) ON DELETE CASCADE"]
    );
    let votes = ReviewVoteV2::for_shop(&db.pool, SHOP);
    assert_eq!(votes.find(vote.id).await.unwrap().unwrap().target_id, review.id);
    // The link still cascades.
    assert!(Review::for_shop(&db.pool, SHOP).delete(review.id).await.unwrap());
    assert_eq!(votes.count().await.unwrap(), 0);
    db.cleanup().await.unwrap();
}

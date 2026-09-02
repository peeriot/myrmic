use super::{open_tmp, write};

use crate::domain::Scope;
use crate::semantic::{Query, Update};
use crate::store::TransactionOptions;
use crate::store::fjall::Transaction;

use anyhow::Context;
use oxrdf::Term;
use spareval::{QueryResults, QuerySolution};

macro_rules! parse_term {
    ($expr:expr) => {{
        let query = format!(
            r"
        PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>

        SELECT ({} as ?term) WHERE {{}}
        ",
            $expr
        );
        let query = Query::parse(&query, None).expect("invalid query");

        let dataset = oxrdf::Dataset::new();
        let eval = spareval::QueryEvaluator::new();
        let query = query.into();

        let result = eval
            .prepare(&query)
            .execute(&dataset)
            .expect(concat!("Unable to eval term: ", $expr));

        let QueryResults::Solutions(solutions) = result else {
            panic!(concat!("Unable to eval term: ", $expr));
        };

        let mut solutions = solutions.collect::<Vec<_>>();
        let solution = solutions.pop();
        let solution = solution.expect(concat!("Invalid term: ", $expr));
        if !solutions.is_empty() {
            panic!(concat!("Expected only a single term: ", $expr));
        }
        solution
            .expect(concat!("Unable to eval term: ", $expr))
            .get("term")
            .expect(concat!("Unable to eval term: ", $expr))
            .clone()
    }};
}

macro_rules! assert_term {
    ($solution:ident, $index:expr, $value:expr) => {{
        let term = parse_term!($value);
        assert_eq!(
            $solution
                .get($index)
                .expect(concat!("Expected to find ", stringify!($index))),
            &term
        );
    }};
}

fn single_select(tx: &mut Transaction, scope: Scope<'_>, query: &str) -> QuerySolution {
    match try_single_select(tx, scope, query) {
        Ok(solution) => solution,
        Err(err) => {
            panic!("{}", err);
        }
    }
}

fn try_single_select(
    tx: &mut Transaction,
    scope: Scope<'_>,
    query: &str,
) -> anyhow::Result<QuerySolution> {
    let mut solutions = collect_solutions(tx, scope, query);
    let solution = solutions.pop().context("Expected one result")?;
    if !solutions.is_empty() {
        anyhow::bail!("Expected _only_ one result");
    }
    Ok(solution)
}

fn collect_solutions(tx: &mut Transaction, scope: Scope<'_>, query: &str) -> Vec<QuerySolution> {
    tx.sem_solution(
        scope,
        Query::parse(query, None).expect("unable to parse query"),
        0,
        100,
    )
    .expect("unable to read db")
    .into()
}

#[tokio::test]
async fn sem_query_test() {
    const INSERT: &str = r#"
        PREFIX purl: <http://purl.org/dc/elements/1.1/>

        INSERT DATA {
          GRAPH <http://example.org/graph1> {
            <http://example.org/book/1> a <http://schema.org/Book> ;
              purl:title "The Great Gobsby" ;
              purl:creator "F. Scott Geraldine" .
          }
        };

        INSERT DATA {
          GRAPH <http://example.org/graph2> {
            <http://example.org/book/1> a <http://schema.org/Book> ;
              purl:title "The Great Git" ;
              purl:creator "F. Scott No Mates" .
          }
        };

        INSERT DATA {
          GRAPH <http://example.org/graph3> {
            <http://example.org/book/1> a <http://schema.org/Book> ;
              purl:title "The Great Gatsby" ;
              purl:creator "F. Scott Fitzgerald" .
          }
        };
    "#;

    const QUERY: &str = "
        PREFIX purl: <http://purl.org/dc/elements/1.1/>

        SELECT ?title ?author
        WHERE {
            GRAPH <http://example.org/graph3> {
                ?subject purl:title ?title;
                    purl:creator ?author.
            }
        }
    ";

    let store = open_tmp();

    let scope = Scope::default();

    let mut tx = store
        .begin_local(&TransactionOptions::write())
        .expect("Unable to begin tx");

    tx.sem_update(
        scope,
        Update::parse(INSERT, None).expect("Unable to parse update"),
    )
    .expect("unable to read db");

    let solutions = single_select(&mut tx, scope, QUERY);

    assert_eq!(solutions.len(), 2);

    assert_term!(solutions, "title", "\"The Great Gatsby\"");
    assert_term!(solutions, "author", "\"F. Scott Fitzgerald\"");
}

#[tokio::test]
async fn sem_insert_and_delete_test() {
    const QUERY_BEFORE: &str = "
        PREFIX purl: <http://purl.org/dc/elements/1.1/>

        SELECT ?title ?creator
        WHERE {
            GRAPH <http://example.org/graph1> {
                ?subject purl:title ?title;
                    purl:creator ?creator.
            }
        }
    ";

    const QUERY_AFTER: &str = "
        PREFIX purl: <http://purl.org/dc/elements/1.1/>

        SELECT ?creator
        WHERE {
            GRAPH <http://example.org/graph1> {
                ?subject purl:creator ?creator.
            }
        }
    ";

    const INSERT: &str = r#"
        INSERT DATA {
          GRAPH <http://example.org/graph1> {
            <http://example.org/book/1> a <http://schema.org/Book> ;
              <http://purl.org/dc/elements/1.1/title> "The Great Gatsby" ;
              <http://purl.org/dc/elements/1.1/creator> "F. Scott Fitzgerald" .
          }
        }
    "#;

    const DELETE: &str = r#"
        DELETE DATA {
          GRAPH <http://example.org/graph1> {
            <http://example.org/book/1> <http://purl.org/dc/elements/1.1/title> "The Great Gatsby".
          }
        }
    "#;

    let store = open_tmp();

    let scope = Scope::default();

    let mut tx = store
        .begin_local(&TransactionOptions::write())
        .expect("Unable to begin tx");

    tx.sem_update(
        scope,
        Update::parse(INSERT, None).expect("Unable to parse update"),
    )
    .expect("unable to read db");

    let solution = single_select(&mut tx, scope, QUERY_BEFORE);
    assert_term!(solution, "title", "\"The Great Gatsby\"");
    assert_term!(solution, "creator", "\"F. Scott Fitzgerald\"");

    tx.sem_update(
        scope,
        Update::parse(DELETE, None).expect("unable to parse update"),
    )
    .expect("unable to read db");

    assert!(try_single_select(&mut tx, scope, QUERY_BEFORE).is_err());
    let solution = single_select(&mut tx, scope, QUERY_AFTER);
    assert_term!(solution, "creator", "\"F. Scott Fitzgerald\"");
}

#[tokio::test]
async fn sem_delete_insert_test() {
    const SETUP: &str = r#"
        INSERT DATA {
            <http://example.org/person1> <http://example.org/name> "Barry" .
            <http://example.org/person1> <http://example.org/age> "30" .
        }
    "#;

    const MODIFY: &str = r#"
        DELETE {
            ?person <http://example.org/age> "30" .
        }
        INSERT {
            ?person <http://example.org/age> "31" .
            ?person <http://example.org/city> "Dresden" .
        }
        WHERE {
            ?person <http://example.org/name> "Barry" .
        }
    "#;

    const SELECT: &str = r#"
        SELECT ?person ?city ?age WHERE {
            ?person <http://example.org/name> "Barry" .
            ?person <http://example.org/age> ?age .
            OPTIONAL {
                ?person <http://example.org/city> ?city .
            }
        }
    "#;

    let store = open_tmp();

    let scope = Scope::default();

    let mut tx = store
        .begin_local(&TransactionOptions::write())
        .expect("Unable to begin tx");

    tx.sem_update(
        scope,
        Update::parse(SETUP, None).expect("unable to parse update"),
    )
    .expect("unable to read db");

    let solution = single_select(&mut tx, scope, SELECT);
    assert_term!(solution, "person", "<http://example.org/person1>");
    assert_term!(solution, "age", "\"30\"");
    assert!(
        solution.get("city").is_none(),
        "`city` should not be present"
    );

    tx.sem_update(
        scope,
        Update::parse(MODIFY, None).expect("unable to parse update"),
    )
    .expect("unable to read db");

    let solution = single_select(&mut tx, scope, SELECT);
    assert_term!(solution, "age", "\"31\"");
    assert_term!(solution, "city", "\"Dresden\"");
}

#[tokio::test]
async fn sem_query_graphs() {
    const INSERT: &str = r#"
        INSERT DATA {
          GRAPH <http://example.org/graph1> {
            <http://example.org/book/1> a <http://schema.org/Book> ;
              <http://purl.org/dc/elements/1.1/title> "The Great Gatsby" ;
              <http://purl.org/dc/elements/1.1/creator> "F. Scott Fitzgerald" .
          }
        };

        INSERT DATA {
          GRAPH <http://example.org/graph2> {
            <http://example.org/book/1> a <http://schema.org/Book> ;
              <http://purl.org/dc/elements/1.1/title> "The Great Gatsby" ;
              <http://purl.org/dc/elements/1.1/creator> "F. Scott Fitzgerald" .
          }
        };

        INSERT DATA {
          GRAPH <http://example.org/graph3> {
            <http://example.org/book/1> a <http://schema.org/Book> ;
              <http://purl.org/dc/elements/1.1/title> "The Great Gatsby" ;
              <http://purl.org/dc/elements/1.1/creator> "F. Scott Fitzgerald" .
          }
        };
    "#;

    const QUERY: &str = r"
        SELECT ?graph
        WHERE {
          GRAPH ?graph {}
        }
    ";

    let store = open_tmp();

    let scope = Scope::default();

    let mut tx = store
        .begin_local(&TransactionOptions::write())
        .expect("Unable to begin tx");

    tx.sem_update(
        scope,
        Update::parse(INSERT, None).expect("Unable to parse update"),
    )
    .expect("unable to read db");

    let solutions = collect_solutions(&mut tx, scope, QUERY);

    let mut names = vec![];

    for solution in solutions {
        let graph_name = solution
            .get("graph")
            .expect("We should have received a result");

        let Term::NamedNode(node) = graph_name else {
            panic!("Expected a named node.");
        };

        names.push(String::from(node.as_str()));
    }

    assert_eq!(names.len(), 3, "Expected 3 graph names.");

    assert!(
        names.contains(&String::from("http://example.org/graph1")),
        "Expected graph1 to be present"
    );
    assert!(
        names.contains(&String::from("http://example.org/graph2")),
        "Expected graph2 to be present"
    );
    assert!(
        names.contains(&String::from("http://example.org/graph3")),
        "Expected graph3 to be present"
    );
}

#[tokio::test]
async fn sem_query_multiple_graphs() {
    const INSERT: &str = r#"
        INSERT DATA {
          GRAPH <http://example.org/graph1> {
            <http://example.org/book/1> a <http://schema.org/Book> ;
              <http://purl.org/dc/elements/1.1/title> "The Great Gobsby" ;
              <http://purl.org/dc/elements/1.1/creator> "F. Scott Fitzgerald" .
          }
        };

        INSERT DATA {
          GRAPH <http://example.org/graph2> {
            <http://example.org/book/1> a <http://schema.org/Book> ;
              <http://purl.org/dc/elements/1.1/title> "The Great Git" ;
              <http://purl.org/dc/elements/1.1/creator> "F. Scott Fitzgerald" .
          }
        };

        INSERT DATA {
          GRAPH <http://example.org/graph3> {
            <http://example.org/book/1> a <http://schema.org/Book> ;
              <http://purl.org/dc/elements/1.1/title> "The Great Gatsby" ;
              <http://purl.org/dc/elements/1.1/creator> "F. Scott Fitzgerald" .
          }
        };
    "#;

    const QUERY: &str = r"
        PREFIX purl: <http://purl.org/dc/elements/1.1/>

        SELECT ?graph ?book ?title
        WHERE {
          GRAPH ?graph {
            ?book purl:title ?title
          }
        }
    ";

    let store = open_tmp();

    let scope = Scope::default();

    let mut tx = store
        .begin_local(&TransactionOptions::write())
        .expect("Unable to begin tx");

    tx.sem_update(
        scope,
        Update::parse(INSERT, None).expect("Unable to parse update"),
    )
    .expect("unable to read db");

    let mut solutions = collect_solutions(&mut tx, scope, QUERY);

    assert_eq!(solutions.len(), 3);

    // Just want to check them in a well-defined order.
    solutions.sort_by(|left, right| {
        let left = left.get("graph").unwrap().to_string();
        let right = right.get("graph").unwrap().to_string();
        right.cmp(&left)
    });

    let solution = solutions.pop().expect("expect 3 solutions");
    assert_term!(solution, "graph", "<http://example.org/graph1>");
    assert_term!(solution, "title", "\"The Great Gobsby\"");

    let solution = solutions.pop().expect("expect 3 solutions");
    assert_term!(solution, "graph", "<http://example.org/graph2>");
    assert_term!(solution, "title", "\"The Great Git\"");

    let solution = solutions.pop().expect("expect 3 solutions");
    assert_term!(solution, "graph", "<http://example.org/graph3>");
    assert_term!(solution, "title", "\"The Great Gatsby\"");
}

#[tokio::test]
async fn test_sparql_from() {
    const INSERT: &str = r#"
        PREFIX foaf: <http://xmlns.com/foaf/0.1/>

        INSERT DATA {
          GRAPH <http://example.org/people> {
            <http://example.org/people#alice> foaf:name "Alice" ;
                     foaf:age 30 .

            <http://example.org/people#bob> foaf:name "Bob" ;
                   foaf:age 29 .
          }
        };
    "#;

    const QUERY: &str = "
        PREFIX foaf: <http://xmlns.com/foaf/0.1/>

        SELECT ?person ?title
        FROM <http://example.org/people>
        WHERE {
            ?person foaf:name ?title
        }
    ";

    let store = open_tmp();

    let scope = Scope::default();

    let mut tx = store
        .begin_local(&TransactionOptions::write())
        .expect("Unable to begin tx");

    tx.sem_update(
        scope,
        Update::parse(INSERT, None).expect("Unable to parse update"),
    )
    .expect("unable to read db");

    let mut solutions = collect_solutions(&mut tx, scope, QUERY);

    assert_eq!(solutions.len(), 2);

    let solution = solutions.pop().expect("expect 2 solutions");
    assert_term!(solution, "person", "<http://example.org/people#bob>");
    assert_term!(solution, "title", "\"Bob\"");

    let solution = solutions.pop().expect("expect 2 solutions");
    assert_term!(solution, "person", "<http://example.org/people#alice>");
    assert_term!(solution, "title", "\"Alice\"");
}

#[tokio::test]
async fn test_sparql_from_multi() {
    const INSERT: &str = r#"
        PREFIX dc: <http://purl.org/dc/elements/1.1/>

        INSERT DATA {
            GRAPH <http://example.org/books/2023> {
                <http://example.org/book#book1> dc:title "Learning SparQL" ;
                                                dc:year 2023 .
            }

            GRAPH <http://example.org/books/2024> {
                <http://example.org/book#book2> dc:title "RDF Fundamentals" ;
                                                dc:year 2024 .
            }
        };
    "#;

    const QUERY: &str = "
        PREFIX dc: <http://purl.org/dc/elements/1.1/>

        SELECT ?book ?title
        FROM <http://example.org/books/2023>
        FROM <http://example.org/books/2024>
        WHERE {
          ?book dc:title ?title .
        }
    ";

    let store = open_tmp();

    let scope = Scope::default();

    let mut tx = store
        .begin_local(&TransactionOptions::write())
        .expect("Unable to begin tx");

    tx.sem_update(
        scope,
        Update::parse(INSERT, None).expect("Unable to parse update"),
    )
    .expect("unable to read db");

    let mut solutions = collect_solutions(&mut tx, scope, QUERY);

    assert_eq!(solutions.len(), 2);

    let solution = solutions.pop().expect("expect 2 solutions");
    assert_term!(solution, "book", "<http://example.org/book#book2>");
    assert_term!(solution, "title", "\"RDF Fundamentals\"");

    let solution = solutions.pop().expect("expect 2 solutions");
    assert_term!(solution, "book", "<http://example.org/book#book1>");
    assert_term!(solution, "title", "\"Learning SparQL\"");
}

#[tokio::test]
async fn test_sparql_from_named() {
    const INSERT: &str = "
        PREFIX ex: <http://example.org/sales/>

        INSERT DATA {
            GRAPH <http://example.org/sales/q1> {
                <http://example.org/product#prod1> ex:price 25.0 ;
                                                   ex:units 100 .
            }

            GRAPH <http://example.org/sales/q2> {
                <http://example.org/product#prod1> ex:price 50.0 ;
                                                   ex:units 150 .
            }
        };
    ";

    const QUERY: &str = "
        PREFIX ex: <http://example.org/sales/>

        SELECT ?quarter ?price ?units
        FROM NAMED <http://example.org/sales/q1>
        FROM NAMED <http://example.org/sales/q2>
        WHERE {
          GRAPH ?quarter {
            <http://example.org/product#prod1> ex:price ?price ;
                     ex:units ?units .
          }
        }
    ";

    let store = open_tmp();

    let scope = Scope::default();

    let mut tx = store
        .begin_local(&TransactionOptions::write())
        .expect("Unable to begin tx");

    tx.sem_update(
        scope,
        Update::parse(INSERT, None).expect("Unable to parse update"),
    )
    .expect("unable to read db");

    let mut solutions = collect_solutions(&mut tx, scope, QUERY);

    assert_eq!(solutions.len(), 2);

    let solution = solutions.pop().expect("expect 2 solutions");
    assert_term!(solution, "quarter", "<http://example.org/sales/q2>");
    assert_term!(solution, "price", "\"50\"^^xsd:decimal");
    assert_term!(solution, "units", "150");

    let solution = solutions.pop().expect("expect 2 solutions");
    assert_term!(solution, "quarter", "<http://example.org/sales/q1>");
    assert_term!(solution, "price", "\"25\"^^xsd:decimal");
    assert_term!(solution, "units", "100");
}

#[tokio::test]
async fn test_sparql_from_named_and_default() {
    const INSERT: &str = r#"
        PREFIX foaf: <http://xmlns.com/foaf/0.1/>

        INSERT DATA {
            GRAPH <http://example.org/employees> {
                <http://example.org/person#emp1> foaf:name "Person 1" ;
                    foaf:department "Software" .

                <http://example.org/person#emp2> foaf:name "Person 2" ;
                    foaf:department "DevOps" .
            }

            GRAPH <http://example.org/salaries> {
                <http://example.org/person#emp1> foaf:salary 973.
                <http://example.org/person#emp2> foaf:salary 932.
            }
        };
    "#;

    const QUERY: &str = "
        PREFIX foaf: <http://xmlns.com/foaf/0.1/>

        SELECT ?name ?dept ?salary
        FROM <http://example.org/employees>
        FROM NAMED <http://example.org/salaries>
        WHERE {
          ?emp foaf:name ?name ;
               foaf:department ?dept .

          GRAPH <http://example.org/salaries> {
            ?emp foaf:salary ?salary .
          }
        }
    ";

    let store = open_tmp();

    let scope = Scope::default();

    let mut tx = store
        .begin_local(&TransactionOptions::write())
        .expect("Unable to begin tx");

    tx.sem_update(
        scope,
        Update::parse(INSERT, None).expect("Unable to parse update"),
    )
    .expect("unable to read db");

    let mut solutions = collect_solutions(&mut tx, scope, QUERY);

    assert_eq!(solutions.len(), 2);

    solutions.sort_by(|left, right| {
        let left = left.get("name").unwrap().to_string();
        let right = right.get("name").unwrap().to_string();
        right.cmp(&left)
    });

    let solution = solutions.pop().expect("expect 2 solutions");
    assert_term!(solution, "name", "\"Person 1\"");
    assert_term!(solution, "dept", "\"Software\"");
    assert_term!(solution, "salary", "973");

    let solution = solutions.pop().expect("expect 2 solutions");
    assert_term!(solution, "name", "\"Person 2\"");
    assert_term!(solution, "dept", "\"DevOps\"");
    assert_term!(solution, "salary", "932");
}

#[tokio::test]
async fn test_sparql_from_named_and_default_missing() {
    const INSERT: &str = r#"
        PREFIX foaf: <http://xmlns.com/foaf/0.1/>
        PREFIX : <http://example.org/>

        INSERT DATA {
            GRAPH <http://example.org/products> {
                <http://example.org/#prod1> :name "Laptop" ;
                         :category "Electronics" .

                <http://example.org/#prod2> :name "Book" ;
                         :category "Media" .

                <http://example.org/#prod3> :name "Shirt" ;
                         :category "Clothing" .
            }

            GRAPH <http://example.org/prices> {
                <http://example.org/#prod1> :price 999 .
                <http://example.org/#prod2> :price 15 .
                # No price for prod 3
            }

            GRAPH <http://example.org/inventory> {
                <http://example.org/#prod1> :inStock 10 .
                # No entry for prod 2
                <http://example.org/#prod3> :inStock 50 .
            }
        };
    "#;

    const QUERY: &str = "
        PREFIX : <http://example.org/>
        PREFIX foaf: <http://xmlns.com/foaf/0.1/>

        SELECT ?product ?name ?price ?stock
        FROM <http://example.org/products>
        FROM NAMED <http://example.org/prices>
        FROM NAMED <http://example.org/inventory>
        WHERE {
          ?product :name ?name ;
                   :category ?category .

          GRAPH <http://example.org/prices> {
            ?product :price ?price .
          }

          GRAPH <http://example.org/inventory> {
            ?product :inStock ?stock .
          }
        }
    ";

    let store = open_tmp();

    let scope = Scope::default();

    let mut tx = store
        .begin_local(&TransactionOptions::write())
        .expect("Unable to begin tx");

    tx.sem_update(
        scope,
        Update::parse(INSERT, None).expect("Unable to parse update"),
    )
    .expect("unable to read db");

    let solution = single_select(&mut tx, scope, QUERY);
    assert_term!(solution, "product", "<http://example.org/#prod1>");
    assert_term!(solution, "name", "\"Laptop\"");
    assert_term!(solution, "price", "\"999\"^^xsd:integer");
    assert_term!(solution, "stock", "\"10\"^^xsd:integer");
}

#[tokio::test]
async fn semantic_literals() {
    const INSERT: &str = include_str!("semantic_literals_insert.rq");
    const QUERY: &str = include_str!("semantic_literals_query.rq");

    let store = open_tmp();

    let scope = Scope::default();

    let mut tx = store
        .begin_local(&TransactionOptions::write())
        .expect("Unable to begin tx");

    tx.sem_update(
        scope,
        Update::parse(INSERT, None).expect("Unable to parse update"),
    )
    .expect("unable to read db");

    let solutions = tx
        .sem_solution(
            scope,
            Query::parse(QUERY, None).expect("unable to parse update"),
            0,
            100,
        )
        .expect("unable to read db");

    let mut solutions: Vec<QuerySolution> = solutions.into();
    let solution = solutions.pop();

    let solution = solution.expect("There should be at least one result.");
    assert!(solutions.is_empty(), "There should be only one result");

    assert_term!(solution, "title", "\"Data Scientist\"@en");
    assert_term!(solution, "titulo", "\"Científica de Datos\"@es");
    assert_term!(solution, "titre", "\"Scientifique des Données\"@fr");
    assert_term!(solution, "age", "\"35\"^^xsd:integer");
    assert_term!(solution, "height", "\"1.68\"^^xsd:decimal");
    assert_term!(solution, "salary", "\"95000\"^^xsd:decimal");
    assert_term!(solution, "tau", "\"6.28\"^^xsd:float");
    assert_term!(solution, "pi", "\"3.14159265359\"^^xsd:double");
    assert_term!(solution, "isActive", "\"true\"^^xsd:boolean");
    assert_term!(solution, "isManager", "\"false\"^^xsd:boolean");
    assert_term!(solution, "startDate", "\"2021-03-01\"^^xsd:date");
    assert_term!(
        solution,
        "lastLogin",
        "\"2024-07-21T16:45:30.123Z\"^^xsd:dateTime"
    );
    assert_term!(solution, "workHours", "\"08:30:00\"^^xsd:time");
    assert_term!(solution, "experience", "\"P10Y6M15D\"^^xsd:duration");
    assert_term!(solution, "birthYear", "\"1989\"^^xsd:gYear");
    assert_term!(solution, "graduationMonth", "\"2011-05\"^^xsd:gYearMonth");
    assert_term!(solution, "favoriteMonth", "\"--06\"^^xsd:gMonth");
    assert_term!(solution, "birthday", "\"---22\"^^xsd:gDay");
    assert_term!(solution, "anniversary", "\"--03-01\"^^xsd:gMonthDay");
    assert_term!(solution, "hexColor", "\"2ECC71\"^^xsd:hexBinary");
    assert_term!(
        solution,
        "avatar",
        "\"aW1hZ2VfZGF0YV9oZXJl\"^^xsd:base64Binary"
    );
    assert_term!(
        solution,
        "portfolio",
        "\"https://janesmith.dev\"^^xsd:anyURI"
    );
    assert_term!(solution, "employeeId", "\"EMP-67890\"^^xsd:token");
    assert_term!(solution, "performanceScore", "\"95\"^^xsd:integer");
    assert_term!(solution, "debtBalance", "\"-1500\"^^xsd:integer");
    assert_term!(solution, "vacationDays", "\"15\"^^xsd:integer");
    assert_term!(solution, "overtimeHours", "\"0\"^^xsd:integer");
    assert_term!(solution, "department", "\"Engineering\"^^xsd:NCName");
    assert_term!(
        solution,
        "phoneNumber",
        "\"+1-555-0123\"^^xsd:normalizedString"
    );
    assert_term!(
        solution,
        "bio",
        r#""""Multi-line biography
            with special chars & symbols!""""#
    );
}

/// One triple in the default graph, two quads in `graph1`, one in `graph2`.
const GRAPHS_SETUP: &str = r#"
    PREFIX purl: <http://purl.org/dc/elements/1.1/>

    INSERT DATA {
        <http://example.org/book/0> purl:title "Default Book" .

        GRAPH <http://example.org/graph1> {
            <http://example.org/book/1> purl:title "Book One" ;
                purl:creator "Alice" .
        }

        GRAPH <http://example.org/graph2> {
            <http://example.org/book/2> purl:title "Book Two" .
        }
    }
"#;

fn apply(tx: &mut Transaction, scope: Scope<'_>, update: &str) {
    try_apply(tx, scope, update).expect("unable to apply update");
}

fn try_apply(tx: &mut Transaction, scope: Scope<'_>, update: &str) -> anyhow::Result<()> {
    let update = Update::parse(update, None).context("unable to parse update")?;

    tx.sem_update(scope, update)
}

/// The default graph alone.
fn count_default(tx: &mut Transaction, scope: Scope<'_>) -> usize {
    collect_solutions(tx, scope, "SELECT ?s ?p ?o WHERE { ?s ?p ?o }").len()
}

/// Every named graph, which never includes the default one.
fn count_named(tx: &mut Transaction, scope: Scope<'_>) -> usize {
    collect_solutions(tx, scope, "SELECT ?s ?p ?o WHERE { GRAPH ?g { ?s ?p ?o } }").len()
}

fn count_graph(tx: &mut Transaction, scope: Scope<'_>, graph: &str) -> usize {
    let query = format!("SELECT ?s ?p ?o WHERE {{ GRAPH <{graph}> {{ ?s ?p ?o }} }}");

    collect_solutions(tx, scope, &query).len()
}

fn graph_names(tx: &mut Transaction, scope: Scope<'_>) -> Vec<String> {
    const QUERY: &str = r"
        SELECT ?graph
        WHERE {
          GRAPH ?graph {}
        }
    ";

    let mut names = collect_solutions(tx, scope, QUERY)
        .into_iter()
        .map(|solution| {
            let Some(Term::NamedNode(node)) = solution.get("graph") else {
                panic!("Expected a named node.");
            };

            String::from(node.as_str())
        })
        .collect::<Vec<_>>();

    names.sort();
    names
}

#[tokio::test]
async fn sem_clear_graph_keeps_the_graph() {
    let store = open_tmp();
    let scope = Scope::default();
    let mut tx = write(&store);

    apply(&mut tx, scope, GRAPHS_SETUP);

    assert_eq!(count_graph(&mut tx, scope, "http://example.org/graph1"), 2);
    assert_eq!(count_named(&mut tx, scope), 3);

    apply(&mut tx, scope, "CLEAR GRAPH <http://example.org/graph1>");

    assert_eq!(count_graph(&mut tx, scope, "http://example.org/graph1"), 0);
    assert_eq!(
        count_graph(&mut tx, scope, "http://example.org/graph2"),
        1,
        "the other named graph should be untouched"
    );
    assert_eq!(
        count_default(&mut tx, scope),
        1,
        "the default graph should be untouched"
    );

    assert_eq!(
        graph_names(&mut tx, scope),
        vec![
            String::from("http://example.org/graph1"),
            String::from("http://example.org/graph2"),
        ],
        "CLEAR leaves the graph itself in place"
    );
}

#[tokio::test]
async fn sem_drop_graph_removes_the_graph() {
    let store = open_tmp();
    let scope = Scope::default();
    let mut tx = write(&store);

    apply(&mut tx, scope, GRAPHS_SETUP);
    apply(&mut tx, scope, "DROP GRAPH <http://example.org/graph1>");

    assert_eq!(count_graph(&mut tx, scope, "http://example.org/graph1"), 0);
    assert_eq!(count_named(&mut tx, scope), 1);
    assert_eq!(count_default(&mut tx, scope), 1);

    assert_eq!(
        graph_names(&mut tx, scope),
        vec![String::from("http://example.org/graph2")],
        "DROP removes the graph itself"
    );
}

#[tokio::test]
async fn sem_clear_default_keeps_named_graphs() {
    let store = open_tmp();
    let scope = Scope::default();
    let mut tx = write(&store);

    apply(&mut tx, scope, GRAPHS_SETUP);
    apply(&mut tx, scope, "CLEAR DEFAULT");

    assert_eq!(
        count_default(&mut tx, scope),
        0,
        "only the default graph triple should be gone"
    );
    assert_eq!(count_named(&mut tx, scope), 3);
    assert_eq!(count_graph(&mut tx, scope, "http://example.org/graph1"), 2);
    assert_eq!(count_graph(&mut tx, scope, "http://example.org/graph2"), 1);
}

#[tokio::test]
async fn sem_clear_named_keeps_the_default_graph() {
    let store = open_tmp();
    let scope = Scope::default();
    let mut tx = write(&store);

    apply(&mut tx, scope, GRAPHS_SETUP);
    apply(&mut tx, scope, "CLEAR NAMED");

    assert_eq!(
        count_default(&mut tx, scope),
        1,
        "the default graph triple should survive"
    );
    assert_eq!(count_named(&mut tx, scope), 0);
    assert_eq!(count_graph(&mut tx, scope, "http://example.org/graph1"), 0);

    assert_eq!(
        graph_names(&mut tx, scope),
        vec![
            String::from("http://example.org/graph1"),
            String::from("http://example.org/graph2"),
        ],
    );
}

#[tokio::test]
async fn sem_drop_named_removes_every_graph() {
    let store = open_tmp();
    let scope = Scope::default();
    let mut tx = write(&store);

    apply(&mut tx, scope, GRAPHS_SETUP);
    apply(&mut tx, scope, "DROP NAMED");

    assert_eq!(
        count_default(&mut tx, scope),
        1,
        "the default graph triple should survive"
    );
    assert_eq!(count_named(&mut tx, scope), 0);
    assert!(graph_names(&mut tx, scope).is_empty());
}

#[tokio::test]
async fn sem_drop_all_empties_the_scope() {
    let store = open_tmp();
    let scope = Scope::default();
    let mut tx = write(&store);

    apply(&mut tx, scope, GRAPHS_SETUP);
    apply(&mut tx, scope, "DROP ALL");

    assert_eq!(count_default(&mut tx, scope), 0);
    assert_eq!(count_named(&mut tx, scope), 0);
    assert!(graph_names(&mut tx, scope).is_empty());
}

#[tokio::test]
async fn sem_clear_all_empties_the_scope_but_keeps_graphs() {
    let store = open_tmp();
    let scope = Scope::default();
    let mut tx = write(&store);

    apply(&mut tx, scope, GRAPHS_SETUP);
    apply(&mut tx, scope, "CLEAR ALL");

    assert_eq!(count_default(&mut tx, scope), 0);
    assert_eq!(count_named(&mut tx, scope), 0);
    assert_eq!(
        graph_names(&mut tx, scope),
        vec![
            String::from("http://example.org/graph1"),
            String::from("http://example.org/graph2"),
        ],
    );
}

#[tokio::test]
async fn sem_drop_missing_graph() {
    const MISSING: &str = "DROP GRAPH <http://example.org/nope>";
    const MISSING_SILENT: &str = "DROP SILENT GRAPH <http://example.org/nope>";
    const MISSING_CLEAR: &str = "CLEAR GRAPH <http://example.org/nope>";

    let store = open_tmp();
    let scope = Scope::default();
    let mut tx = write(&store);

    apply(&mut tx, scope, GRAPHS_SETUP);

    assert!(
        try_apply(&mut tx, scope, MISSING).is_err(),
        "dropping a missing graph should fail"
    );
    assert!(
        try_apply(&mut tx, scope, MISSING_CLEAR).is_err(),
        "clearing a missing graph should fail"
    );

    try_apply(&mut tx, scope, MISSING_SILENT).expect("SILENT should swallow the error");

    assert_eq!(
        count_default(&mut tx, scope),
        1,
        "nothing should have changed"
    );
    assert_eq!(
        count_named(&mut tx, scope),
        3,
        "nothing should have changed"
    );
}

#[tokio::test]
async fn sem_load_is_unsupported() {
    const LOAD: &str = "LOAD <http://example.org/data.ttl>";
    const LOAD_SILENT: &str = "LOAD SILENT <http://example.org/data.ttl>";

    let store = open_tmp();
    let scope = Scope::default();
    let mut tx = write(&store);

    apply(&mut tx, scope, GRAPHS_SETUP);

    assert!(try_apply(&mut tx, scope, LOAD).is_err());
    try_apply(&mut tx, scope, LOAD_SILENT).expect("SILENT should swallow the error");

    assert_eq!(
        count_default(&mut tx, scope),
        1,
        "nothing should have changed"
    );
    assert_eq!(
        count_named(&mut tx, scope),
        3,
        "nothing should have changed"
    );
}

/// `Some(None)`, `Some(Some(_))` and `None` are three distinct graph selections;
/// the default graph is not part of the named graph space, or vice versa.
#[tokio::test]
async fn sem_default_graph_is_separate_from_named_graphs() {
    let store = open_tmp();
    let scope = Scope::default();
    let mut tx = write(&store);

    apply(&mut tx, scope, GRAPHS_SETUP);

    let solution = single_select(
        &mut tx,
        scope,
        "SELECT ?title WHERE { ?book <http://purl.org/dc/elements/1.1/title> ?title }",
    );
    assert_term!(solution, "title", "\"Default Book\"");

    assert_eq!(
        count_default(&mut tx, scope),
        1,
        "a bare pattern should not reach into the named graphs"
    );
    assert_eq!(
        count_named(&mut tx, scope),
        3,
        "a graph variable should not reach into the default graph"
    );
    assert_eq!(count_graph(&mut tx, scope, "http://example.org/graph1"), 2);
}

/// Removal and insertion land on the same transaction timestamp, so the store's
/// ordering rules have to resolve them by operation order, not by key order.
#[tokio::test]
async fn sem_drop_and_insert_in_one_update() {
    const DROP_THEN_INSERT: &str = r#"
        PREFIX purl: <http://purl.org/dc/elements/1.1/>

        DROP GRAPH <http://example.org/graph1>;

        INSERT DATA {
            GRAPH <http://example.org/graph1> {
                <http://example.org/book/3> purl:title "Book Three" .
            }
        }
    "#;

    const INSERT_THEN_CLEAR: &str = r#"
        PREFIX purl: <http://purl.org/dc/elements/1.1/>

        INSERT DATA {
            GRAPH <http://example.org/graph3> {
                <http://example.org/book/4> purl:title "Book Four" .
            }
        };

        CLEAR GRAPH <http://example.org/graph3>
    "#;

    let store = open_tmp();
    let scope = Scope::default();
    let mut tx = write(&store);

    apply(&mut tx, scope, GRAPHS_SETUP);
    apply(&mut tx, scope, DROP_THEN_INSERT);

    assert_eq!(
        count_graph(&mut tx, scope, "http://example.org/graph1"),
        1,
        "the insert should survive the preceding drop"
    );

    apply(&mut tx, scope, INSERT_THEN_CLEAR);

    assert_eq!(
        count_graph(&mut tx, scope, "http://example.org/graph3"),
        0,
        "the clear should remove the preceding insert"
    );
}

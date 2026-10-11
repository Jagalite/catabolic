// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Frozen graphql-core document-validation regressions. Expected errors were
//! captured with the reference 453fca9 LOCAL_SDL and installed Python 3.14.6.
//! These focused cases do not claim all GraphQL static-validation rules match.
use catabolic_store::{Store, graphql, migration};
use serde_json::{Value, json};

#[test]
fn frozen_document_errors_precede_operation_selection_and_variable_coercion() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("validation.db");
    migration::initialize(&path).unwrap();
    let mut store = Store::open(&path, true, false).unwrap();
    let cases: Value = serde_json::from_str(CASES).unwrap();
    let mut failures = vec![];
    for case in cases.as_array().unwrap() {
        let (returned, report) = graphql::execute_store(
            store,
            case["document"].as_str().unwrap(),
            "default",
            json!({"n":2147483648i64}),
            Some("Missing"),
            5000,
        )
        .unwrap();
        store = returned;
        if report["errors"] != case["errors"] {
            failures.push(format!(
                "{}: expected {}, got {}",
                case["name"], case["errors"], report["errors"]
            ));
        }
        assert_eq!(report["data"], Value::Null);
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
const CASES: &str = r##"[
  {
    "name": "unknown-field-plus-unused-variable",
    "document": "query($n:Int!){notAField}",
    "errors": [
      {
        "message": "Cannot query field 'notAField' on type 'Query'.",
        "locations": [
          {
            "line": 1,
            "column": 16
          }
        ]
      },
      {
        "message": "Variable '$n' is never used.",
        "locations": [
          {
            "line": 1,
            "column": 7
          }
        ]
      }
    ]
  },
  {
    "name": "named-unused-order",
    "document": "query A($z:String,$n:Int!){notAField} query B($x:Int){bad}",
    "errors": [
      {
        "message": "Cannot query field 'notAField' on type 'Query'.",
        "locations": [
          {
            "line": 1,
            "column": 28
          }
        ]
      },
      {
        "message": "Variable '$z' is never used in operation 'A'.",
        "locations": [
          {
            "line": 1,
            "column": 9
          }
        ]
      },
      {
        "message": "Variable '$n' is never used in operation 'A'.",
        "locations": [
          {
            "line": 1,
            "column": 19
          }
        ]
      },
      {
        "message": "Cannot query field 'bad' on type 'Query'.",
        "locations": [
          {
            "line": 1,
            "column": 55
          }
        ]
      },
      {
        "message": "Variable '$x' is never used in operation 'B'.",
        "locations": [
          {
            "line": 1,
            "column": 47
          }
        ]
      }
    ]
  },
  {
    "name": "fragment-before-operation",
    "document": "fragment F on Query { notAField } query A($x:Int){...F}",
    "errors": [
      {
        "message": "Cannot query field 'notAField' on type 'Query'.",
        "locations": [
          {
            "line": 1,
            "column": 23
          }
        ]
      },
      {
        "message": "Variable '$x' is never used in operation 'A'.",
        "locations": [
          {
            "line": 1,
            "column": 43
          }
        ]
      }
    ]
  },
  {
    "name": "unknown-variable-type",
    "document": "query($n:MissingType){items(first:$n){nodes{id}}}",
    "errors": [
      {
        "message": "Unknown type 'MissingType'.",
        "locations": [
          {
            "line": 1,
            "column": 10
          }
        ]
      }
    ]
  },
  {
    "name": "type-suggestion",
    "document": "query($n:Strin){notAField}",
    "errors": [
      {
        "message": "Unknown type 'Strin'. Did you mean 'String'?",
        "locations": [
          {
            "line": 1,
            "column": 10
          }
        ]
      },
      {
        "message": "Cannot query field 'notAField' on type 'Query'.",
        "locations": [
          {
            "line": 1,
            "column": 17
          }
        ]
      },
      {
        "message": "Variable '$n' is never used.",
        "locations": [
          {
            "line": 1,
            "column": 7
          }
        ]
      }
    ]
  },
  {
    "name": "wrapped-unknown-type",
    "document": "query($n:[UnknownType!]!){items(first:$n){nodes{id}}}",
    "errors": [
      {
        "message": "Unknown type 'UnknownType'.",
        "locations": [
          {
            "line": 1,
            "column": 11
          }
        ]
      }
    ]
  },
  {
    "name": "output-variable-type",
    "document": "query($n:Query){items(first:$n){nodes{id}}}",
    "errors": [
      {
        "message": "Variable '$n' cannot be non-input type 'Query'.",
        "locations": [
          {
            "line": 1,
            "column": 10
          }
        ]
      },
      {
        "message": "Variable '$n' of type 'Query' used in position expecting type 'Int!'.",
        "locations": [
          {
            "line": 1,
            "column": 7
          },
          {
            "line": 1,
            "column": 29
          }
        ]
      }
    ]
  },
  {
    "name": "scalar-incompatible-variable",
    "document": "query($n:String!){items(first:$n){nodes{id}}}",
    "errors": [
      {
        "message": "Variable '$n' of type 'String!' used in position expecting type 'Int!'.",
        "locations": [
          {
            "line": 1,
            "column": 7
          },
          {
            "line": 1,
            "column": 31
          }
        ]
      }
    ]
  },
  {
    "name": "list-incompatible-variable",
    "document": "query($n:[Int!]!){items(first:$n){nodes{id}}}",
    "errors": [
      {
        "message": "Variable '$n' of type '[Int!]!' used in position expecting type 'Int!'.",
        "locations": [
          {
            "line": 1,
            "column": 7
          },
          {
            "line": 1,
            "column": 31
          }
        ]
      }
    ]
  },
  {
    "name": "nullable-required-variable",
    "document": "query($x:Boolean){profile @include(if:$x)}",
    "errors": [
      {
        "message": "Variable '$x' of type 'Boolean' used in position expecting type 'Boolean!'.",
        "locations": [
          {
            "line": 1,
            "column": 7
          },
          {
            "line": 1,
            "column": 39
          }
        ]
      }
    ]
  },
  {
    "name": "null-default-required-variable",
    "document": "query($x:Boolean=null){profile @include(if:$x)}",
    "errors": [
      {
        "message": "Variable '$x' of type 'Boolean' used in position expecting type 'Boolean!'.",
        "locations": [
          {
            "line": 1,
            "column": 7
          },
          {
            "line": 1,
            "column": 44
          }
        ]
      }
    ]
  },
  {
    "name": "incompatible-default",
    "document": "query($n:Int=\"bad\"){items(first:$n){nodes{id}}}",
    "errors": [
      {
        "message": "Int cannot represent non-integer value: \"bad\"",
        "locations": [
          {
            "line": 1,
            "column": 14
          }
        ]
      }
    ]
  },
  {
    "name": "unused-incompatible-default",
    "document": "query($n:Int=\"bad\"){profile}",
    "errors": [
      {
        "message": "Int cannot represent non-integer value: \"bad\"",
        "locations": [
          {
            "line": 1,
            "column": 14
          }
        ]
      },
      {
        "message": "Variable '$n' is never used.",
        "locations": [
          {
            "line": 1,
            "column": 7
          }
        ]
      }
    ]
  },
  {
    "name": "list-default-items",
    "document": "query($x:[String!]=[1,null]){items(tags:$x){nodes{id}}}",
    "errors": [
      {
        "message": "String cannot represent a non string value: 1",
        "locations": [
          {
            "line": 1,
            "column": 21
          }
        ]
      },
      {
        "message": "Expected value of type 'String!', found null.",
        "locations": [
          {
            "line": 1,
            "column": 23
          }
        ]
      }
    ]
  },
  {
    "name": "object-default-fields",
    "document": "query($x:RenditionFilter={current:1,unknown:1}){renditions(matching:$x){nodes}}",
    "errors": [
      {
        "message": "Boolean cannot represent a non boolean value: 1",
        "locations": [
          {
            "line": 1,
            "column": 35
          }
        ]
      },
      {
        "message": "Field 'unknown' is not defined by type 'RenditionFilter'.",
        "locations": [
          {
            "line": 1,
            "column": 37
          }
        ]
      }
    ]
  },
  {
    "name": "nested-variable-type",
    "document": "query($x:Int){renditions(matching:{current:$x}){nodes}}",
    "errors": [
      {
        "message": "Variable '$x' of type 'Int' used in position expecting type 'Boolean'.",
        "locations": [
          {
            "line": 1,
            "column": 7
          },
          {
            "line": 1,
            "column": 44
          }
        ]
      }
    ]
  },
  {
    "name": "nested-list-variable-type",
    "document": "query($x:Int){items(tags:[$x]){nodes{id}}}",
    "errors": [
      {
        "message": "Variable '$x' of type 'Int' used in position expecting type 'String!'.",
        "locations": [
          {
            "line": 1,
            "column": 7
          },
          {
            "line": 1,
            "column": 27
          }
        ]
      }
    ]
  },
  {
    "name": "nested-multiline-locations",
    "document": "query($x:Int){items(tags:[\"a\",\n # skip $fake\n $x]){nodes{id}}}",
    "errors": [
      {
        "message": "Variable '$x' of type 'Int' used in position expecting type 'String!'.",
        "locations": [
          {
            "line": 1,
            "column": 7
          },
          {
            "line": 3,
            "column": 2
          }
        ]
      }
    ]
  },
  {
    "name": "undefined-variable",
    "document": "query A{items(first:$n){nodes{id}}}",
    "errors": [
      {
        "message": "Variable '$n' is not defined by operation 'A'.",
        "locations": [
          {
            "line": 1,
            "column": 21
          },
          {
            "line": 1,
            "column": 1
          }
        ]
      }
    ]
  },
  {
    "name": "undefined-and-unused",
    "document": "query A($m:Int){items(first:$n){nodes{id}}}",
    "errors": [
      {
        "message": "Variable '$n' is not defined by operation 'A'.",
        "locations": [
          {
            "line": 1,
            "column": 29
          },
          {
            "line": 1,
            "column": 1
          }
        ]
      },
      {
        "message": "Variable '$m' is never used in operation 'A'.",
        "locations": [
          {
            "line": 1,
            "column": 9
          }
        ]
      }
    ]
  },
  {
    "name": "unknown-field-variable-is-used",
    "document": "query A($n:Int){notAField(arg:{nested:[$n]})}",
    "errors": [
      {
        "message": "Cannot query field 'notAField' on type 'Query'.",
        "locations": [
          {
            "line": 1,
            "column": 17
          }
        ]
      }
    ]
  },
  {
    "name": "unknown-directive-variable-is-used",
    "document": "query A($n:Int){profile @nope(arg:{nested:[$n]})}",
    "errors": [
      {
        "message": "Unknown directive '@nope'.",
        "locations": [
          {
            "line": 1,
            "column": 25
          }
        ]
      }
    ]
  },
  {
    "name": "unknown-subtree-variable-is-used",
    "document": "query A($n:Int){notAField{other(arg:$n)}}",
    "errors": [
      {
        "message": "Cannot query field 'notAField' on type 'Query'.",
        "locations": [
          {
            "line": 1,
            "column": 17
          }
        ]
      }
    ]
  },
  {
    "name": "fragment-transitive-variable-error",
    "document": "query A($n:String){...F} fragment F on Query{...G} fragment G on Query{items(first:$n){nodes{id}}}",
    "errors": [
      {
        "message": "Variable '$n' of type 'String' used in position expecting type 'Int!'.",
        "locations": [
          {
            "line": 1,
            "column": 9
          },
          {
            "line": 1,
            "column": 84
          }
        ]
      }
    ]
  },
  {
    "name": "fragment-undefined-variable",
    "document": "query A{...F} fragment F on Query{...G} fragment G on Query{items(first:$n){nodes{id}}}",
    "errors": [
      {
        "message": "Variable '$n' is not defined by operation 'A'.",
        "locations": [
          {
            "line": 1,
            "column": 73
          },
          {
            "line": 1,
            "column": 1
          }
        ]
      }
    ]
  },
  {
    "name": "fragment-variable-directive",
    "document": "query A($x:Int){...F} fragment F on Query{profile @include(if:$x)}",
    "errors": [
      {
        "message": "Variable '$x' of type 'Int' used in position expecting type 'Boolean!'.",
        "locations": [
          {
            "line": 1,
            "column": 9
          },
          {
            "line": 1,
            "column": 63
          }
        ]
      }
    ]
  },
  {
    "name": "fragment-spread-directive",
    "document": "query A($x:Int){...F @include(if:$x)} fragment F on Query{profile}",
    "errors": [
      {
        "message": "Variable '$x' of type 'Int' used in position expecting type 'Boolean!'.",
        "locations": [
          {
            "line": 1,
            "column": 9
          },
          {
            "line": 1,
            "column": 34
          }
        ]
      }
    ]
  },
  {
    "name": "inline-fragment-directive",
    "document": "query A($x:Int){... @include(if:$x){profile}}",
    "errors": [
      {
        "message": "Variable '$x' of type 'Int' used in position expecting type 'Boolean!'.",
        "locations": [
          {
            "line": 1,
            "column": 9
          },
          {
            "line": 1,
            "column": 33
          }
        ]
      }
    ]
  },
  {
    "name": "unused-fragment-variable",
    "document": "query A($n:Int){profile} fragment F on Query{items(first:$n){nodes{id}}}",
    "errors": [
      {
        "message": "Variable '$n' is never used in operation 'A'.",
        "locations": [
          {
            "line": 1,
            "column": 9
          }
        ]
      },
      {
        "message": "Fragment 'F' is never used.",
        "locations": [
          {
            "line": 1,
            "column": 26
          }
        ]
      }
    ]
  },
  {
    "name": "operation-directive",
    "document": "query A($x:Int) @include(if:$x){profile}",
    "errors": [
      {
        "message": "Directive '@include' may not be used on query.",
        "locations": [
          {
            "line": 1,
            "column": 17
          }
        ]
      },
      {
        "message": "Variable '$x' of type 'Int' used in position expecting type 'Boolean!'.",
        "locations": [
          {
            "line": 1,
            "column": 9
          },
          {
            "line": 1,
            "column": 29
          }
        ]
      }
    ]
  },
  {
    "name": "variable-directive",
    "document": "query A($x:Int @skip(if:1)){items(first:$x){nodes{id}}}",
    "errors": [
      {
        "message": "Directive '@skip' may not be used on variable definition.",
        "locations": [
          {
            "line": 1,
            "column": 16
          }
        ]
      },
      {
        "message": "Boolean cannot represent a non boolean value: 1",
        "locations": [
          {
            "line": 1,
            "column": 25
          }
        ]
      }
    ]
  },
  {
    "name": "unknown-directive",
    "document": "{profile @unknown}",
    "errors": [
      {
        "message": "Unknown directive '@unknown'.",
        "locations": [
          {
            "line": 1,
            "column": 10
          }
        ]
      }
    ]
  },
  {
    "name": "directive-missing-argument",
    "document": "{profile @include}",
    "errors": [
      {
        "message": "Directive '@include' argument 'if' of type 'Boolean!' is required, but it was not provided.",
        "locations": [
          {
            "line": 1,
            "column": 10
          }
        ]
      }
    ]
  },
  {
    "name": "directive-unknown-argument",
    "document": "{profile @include(unless:1)}",
    "errors": [
      {
        "message": "Unknown argument 'unless' on directive '@include'.",
        "locations": [
          {
            "line": 1,
            "column": 19
          }
        ]
      },
      {
        "message": "Directive '@include' argument 'if' of type 'Boolean!' is required, but it was not provided.",
        "locations": [
          {
            "line": 1,
            "column": 10
          }
        ]
      }
    ]
  },
  {
    "name": "directive-invalid-literal",
    "document": "{profile @include(if:1)}",
    "errors": [
      {
        "message": "Boolean cannot represent a non boolean value: 1",
        "locations": [
          {
            "line": 1,
            "column": 22
          }
        ]
      }
    ]
  },
  {
    "name": "directive-repeated",
    "document": "{profile @include(if:1) @include(if:2)}",
    "errors": [
      {
        "message": "The directive '@include' can only be used once at this location.",
        "locations": [
          {
            "line": 1,
            "column": 10
          },
          {
            "line": 1,
            "column": 25
          }
        ]
      },
      {
        "message": "Boolean cannot represent a non boolean value: 1",
        "locations": [
          {
            "line": 1,
            "column": 22
          }
        ]
      },
      {
        "message": "Boolean cannot represent a non boolean value: 2",
        "locations": [
          {
            "line": 1,
            "column": 37
          }
        ]
      }
    ]
  },
  {
    "name": "directive-duplicate-argument",
    "document": "{profile @include(if:true,if:false)}",
    "errors": [
      {
        "message": "There can be only one argument named 'if'.",
        "locations": [
          {
            "line": 1,
            "column": 19
          },
          {
            "line": 1,
            "column": 27
          }
        ]
      }
    ]
  },
  {
    "name": "duplicate-variable",
    "document": "query($n:Int,$n:String){items(first:$n){nodes{id}}}",
    "errors": [
      {
        "message": "There can be only one variable named '$n'.",
        "locations": [
          {
            "line": 1,
            "column": 8
          },
          {
            "line": 1,
            "column": 15
          }
        ]
      },
      {
        "message": "Variable '$n' of type 'String' used in position expecting type 'Int!'.",
        "locations": [
          {
            "line": 1,
            "column": 14
          },
          {
            "line": 1,
            "column": 37
          }
        ]
      }
    ]
  },
  {
    "name": "duplicate-field-argument",
    "document": "{items(first:1,first:2){nodes{id}}}",
    "errors": [
      {
        "message": "There can be only one argument named 'first'.",
        "locations": [
          {
            "line": 1,
            "column": 8
          },
          {
            "line": 1,
            "column": 16
          }
        ]
      }
    ]
  },
  {
    "name": "required-field-argument",
    "document": "{file{id}}",
    "errors": [
      {
        "message": "Field 'file' argument 'id' of type 'ID!' is required, but it was not provided.",
        "locations": [
          {
            "line": 1,
            "column": 2
          }
        ]
      }
    ]
  },
  {
    "name": "required-field-order",
    "document": "{file{bad}}",
    "errors": [
      {
        "message": "Cannot query field 'bad' on type 'File'. Did you mean 'id'?",
        "locations": [
          {
            "line": 1,
            "column": 7
          }
        ]
      },
      {
        "message": "Field 'file' argument 'id' of type 'ID!' is required, but it was not provided.",
        "locations": [
          {
            "line": 1,
            "column": 2
          }
        ]
      }
    ]
  },
  {
    "name": "nested-input-object-literal",
    "document": "{renditions(matching:{current:1,curent:true}){nodes}}",
    "errors": [
      {
        "message": "Boolean cannot represent a non boolean value: 1",
        "locations": [
          {
            "line": 1,
            "column": 31
          }
        ]
      },
      {
        "message": "Field 'curent' is not defined by type 'RenditionFilter'. Did you mean 'current'?",
        "locations": [
          {
            "line": 1,
            "column": 33
          }
        ]
      }
    ]
  },
  {
    "name": "nested-list-literal",
    "document": "{items(tags:[1,null]){nodes{id}}}",
    "errors": [
      {
        "message": "String cannot represent a non string value: 1",
        "locations": [
          {
            "line": 1,
            "column": 14
          }
        ]
      },
      {
        "message": "Expected value of type 'String!', found null.",
        "locations": [
          {
            "line": 1,
            "column": 16
          }
        ]
      }
    ]
  },
  {
    "name": "enum-string-literal",
    "document": "{items(sort:\"ID\"){nodes{id}}}",
    "errors": [
      {
        "message": "Enum 'ItemSort' cannot represent non-enum value: \"ID\". Did you mean the enum value 'ID'?",
        "locations": [
          {
            "line": 1,
            "column": 13
          }
        ]
      }
    ]
  },
  {
    "name": "enum-bool-literal",
    "document": "{items(sort:true){nodes{id}}}",
    "errors": [
      {
        "message": "Enum 'ItemSort' cannot represent non-enum value: true.",
        "locations": [
          {
            "line": 1,
            "column": 13
          }
        ]
      }
    ]
  },
  {
    "name": "enum-unknown-literal",
    "document": "{items(sort:NAME){nodes{id}}}",
    "errors": [
      {
        "message": "Value 'NAME' does not exist in 'ItemSort' enum.",
        "locations": [
          {
            "line": 1,
            "column": 13
          }
        ]
      }
    ]
  },
  {
    "name": "input-object-scalar-literal",
    "document": "{renditions(matching:1){nodes}}",
    "errors": [
      {
        "message": "Expected value of type 'RenditionFilter', found 1.",
        "locations": [
          {
            "line": 1,
            "column": 22
          }
        ]
      }
    ]
  },
  {
    "name": "unused-fragment",
    "document": "{profile} fragment F on Query{profile}",
    "errors": [
      {
        "message": "Fragment 'F' is never used.",
        "locations": [
          {
            "line": 1,
            "column": 11
          }
        ]
      }
    ]
  },
  {
    "name": "fragment-unknown-type",
    "document": "{...F} fragment F on UnknownType{profile}",
    "errors": [
      {
        "message": "Unknown type 'UnknownType'.",
        "locations": [
          {
            "line": 1,
            "column": 22
          }
        ]
      }
    ]
  },
  {
    "name": "fragment-scalar-type",
    "document": "{...F} fragment F on String{length}",
    "errors": [
      {
        "message": "Fragment 'F' cannot condition on non composite type 'String'.",
        "locations": [
          {
            "line": 1,
            "column": 22
          }
        ]
      }
    ]
  },
  {
    "name": "inline-unknown-type",
    "document": "{... on UnknownType{profile}}",
    "errors": [
      {
        "message": "Unknown type 'UnknownType'.",
        "locations": [
          {
            "line": 1,
            "column": 9
          }
        ]
      }
    ]
  },
  {
    "name": "inline-scalar-type",
    "document": "{... on String{length}}",
    "errors": [
      {
        "message": "Fragment cannot condition on non composite type 'String'.",
        "locations": [
          {
            "line": 1,
            "column": 9
          }
        ]
      }
    ]
  },
  {
    "name": "alias-field-location",
    "document": "{myAlias: notAField}",
    "errors": [
      {
        "message": "Cannot query field 'notAField' on type 'Query'.",
        "locations": [
          {
            "line": 1,
            "column": 2
          }
        ]
      }
    ]
  },
  {
    "name": "impossible-inline-spread",
    "document": "{... on Item{id}}",
    "errors": [
      {
        "message": "Fragment cannot be spread here as objects of type 'Query' can never be of type 'Item'.",
        "locations": [
          {
            "line": 1,
            "column": 2
          }
        ]
      }
    ]
  },
  {
    "name": "impossible-fragment-spread",
    "document": "{...F} fragment F on Item{id}",
    "errors": [
      {
        "message": "Fragment 'F' cannot be spread here as objects of type 'Query' can never be of type 'Item'.",
        "locations": [
          {
            "line": 1,
            "column": 2
          }
        ]
      }
    ]
  },
  {
    "name": "invalid-introspection-leaf",
    "document": "{__schema}",
    "errors": [
      {
        "message": "Field '__schema' of type '__Schema!' must have a selection of subfields. Did you mean '__schema { ... }'?",
        "locations": [
          {
            "line": 1,
            "column": 2
          }
        ]
      }
    ]
  },
  {
    "name": "nested-sibling-fragment-order",
    "document": "query A{items{...F} files{...G}} fragment F on ItemPage{nodes{id} pageInfo @include(if:$f){hasNextPage}} fragment G on FilePage{nodes{id} pageInfo @include(if:$g){hasNextPage}}",
    "errors": [
      {
        "message": "Variable '$g' is not defined by operation 'A'.",
        "locations": [
          {
            "line": 1,
            "column": 160
          },
          {
            "line": 1,
            "column": 1
          }
        ]
      },
      {
        "message": "Variable '$f' is not defined by operation 'A'.",
        "locations": [
          {
            "line": 1,
            "column": 88
          },
          {
            "line": 1,
            "column": 1
          }
        ]
      }
    ]
  },
  {
    "name": "error-limit",
    "document": "query($v0:Int,$v1:Int,$v2:Int,$v3:Int,$v4:Int,$v5:Int,$v6:Int,$v7:Int,$v8:Int,$v9:Int,$v10:Int,$v11:Int,$v12:Int,$v13:Int,$v14:Int,$v15:Int,$v16:Int,$v17:Int,$v18:Int,$v19:Int,$v20:Int,$v21:Int){profile}",
    "errors": [
      {
        "message": "Variable '$v0' is never used.",
        "locations": [
          {
            "line": 1,
            "column": 7
          }
        ]
      },
      {
        "message": "Variable '$v1' is never used.",
        "locations": [
          {
            "line": 1,
            "column": 15
          }
        ]
      },
      {
        "message": "Variable '$v2' is never used.",
        "locations": [
          {
            "line": 1,
            "column": 23
          }
        ]
      },
      {
        "message": "Variable '$v3' is never used.",
        "locations": [
          {
            "line": 1,
            "column": 31
          }
        ]
      },
      {
        "message": "Variable '$v4' is never used.",
        "locations": [
          {
            "line": 1,
            "column": 39
          }
        ]
      },
      {
        "message": "Variable '$v5' is never used.",
        "locations": [
          {
            "line": 1,
            "column": 47
          }
        ]
      },
      {
        "message": "Variable '$v6' is never used.",
        "locations": [
          {
            "line": 1,
            "column": 55
          }
        ]
      },
      {
        "message": "Variable '$v7' is never used.",
        "locations": [
          {
            "line": 1,
            "column": 63
          }
        ]
      },
      {
        "message": "Variable '$v8' is never used.",
        "locations": [
          {
            "line": 1,
            "column": 71
          }
        ]
      },
      {
        "message": "Variable '$v9' is never used.",
        "locations": [
          {
            "line": 1,
            "column": 79
          }
        ]
      },
      {
        "message": "Variable '$v10' is never used.",
        "locations": [
          {
            "line": 1,
            "column": 87
          }
        ]
      },
      {
        "message": "Variable '$v11' is never used.",
        "locations": [
          {
            "line": 1,
            "column": 96
          }
        ]
      },
      {
        "message": "Variable '$v12' is never used.",
        "locations": [
          {
            "line": 1,
            "column": 105
          }
        ]
      },
      {
        "message": "Variable '$v13' is never used.",
        "locations": [
          {
            "line": 1,
            "column": 114
          }
        ]
      },
      {
        "message": "Variable '$v14' is never used.",
        "locations": [
          {
            "line": 1,
            "column": 123
          }
        ]
      },
      {
        "message": "Variable '$v15' is never used.",
        "locations": [
          {
            "line": 1,
            "column": 132
          }
        ]
      },
      {
        "message": "Variable '$v16' is never used.",
        "locations": [
          {
            "line": 1,
            "column": 141
          }
        ]
      },
      {
        "message": "Variable '$v17' is never used.",
        "locations": [
          {
            "line": 1,
            "column": 150
          }
        ]
      },
      {
        "message": "Variable '$v18' is never used.",
        "locations": [
          {
            "line": 1,
            "column": 159
          }
        ]
      },
      {
        "message": "Variable '$v19' is never used.",
        "locations": [
          {
            "line": 1,
            "column": 168
          }
        ]
      },
      {
        "message": "Too many validation errors, error limit reached. Validation aborted."
      }
    ]
  },
  {
    "name": "all-document-before-selection",
    "document": "query A{profile} query B($n:Int=\"bad\"){items(first:$n){nodes{id}}}",
    "errors": [
      {
        "message": "Int cannot represent non-integer value: \"bad\"",
        "locations": [
          {
            "line": 1,
            "column": 33
          }
        ]
      }
    ]
  }
]"##;

#[test]
fn valid_variable_uses_include_nested_values_directives_and_transitive_fragments() {
    for document in [
        "query($n:Int){items(first:$n){nodes{id}}}",
        "query($x:Boolean=true){profile @include(if:$x)}",
        "query($x:Boolean!){profile @include(if:$x)}",
        "query($x:String!){items(tags:[$x]){nodes{id}}}",
        "query($x:Boolean){renditions(matching:{current:$x}){nodes}}",
        "query($x:Boolean!){...F} fragment F on Query{...G} fragment G on Query{profile @include(if:$x)}",
        "query A($x:Boolean!){...F @include(if:$x)} query B{profile} fragment F on Query{profile}",
        "query($x:Boolean!){... @include(if:$x){profile}}",
        "query($x:String!){items(tags:[\"not $unused\", # $comment\n $x]){nodes{id}}}",
        "query($x:String!){items(tags:[\"\"\"not $unused\n and \\\"\"\" still a string\"\"\",$x]){nodes{id}}}",
        "query($x:[String!]=[\"a\"]){items(tags:$x){nodes{id}}}",
        "query($x:RenditionFilter={current:true}){renditions(matching:$x){nodes}}",
        "fragment F on Query{profile @include(if:$x)} query($x:Boolean!){...F}",
    ] {
        let result = graphql::validate_before_context(document, &json!({}), 5000, false).unwrap();
        assert!(result.is_none(), "{document}: {result:?}");
    }
}

#[test]
fn published_http_schema_rejects_local_extension_before_variable_checks() {
    let result = graphql::validate_before_context(
        "query($x:Int!){workInbox(first:$x){nodes}}",
        &json!({"x":"bad"}),
        5000,
        true,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        result["errors"],
        json!([{
            "message":"Cannot query field 'workInbox' on type 'Query'. Did you mean 'worklog'?",
            "locations":[{"line":1,"column":16}]
        }])
    );
}

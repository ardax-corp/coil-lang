// A derive keeps its field attributes when an attribute macro on the same
// type re-emits it (`${raw(t.source)}`), in either order (#1846).
use tests::assert;
use derive_macros::{with_describe, FieldNames};

#[with_describe(prefix = "row")]
#[derive(FieldNames)]
class Row {
    #[names(rename = "row_id")]
    pub id: int,
    pub label: string,
}

#[derive(FieldNames)]
#[with_describe(prefix = "col")]
class Col {
    #[names(rename = "col_id")]
    pub id: int,
}

test("attribute macro first") {
    assert!(Row::field_names()[0] == "row_id");
    assert!(Row::field_names()[1] == "label");
    assert!(new Row(1, "a").describe() == "row:Row");
}

test("derive first") {
    assert!(Col::field_names()[0] == "col_id");
    assert!(new Col(2).describe() == "col:Col");
}

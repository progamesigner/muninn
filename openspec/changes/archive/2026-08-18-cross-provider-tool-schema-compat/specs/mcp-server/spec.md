## ADDED Requirements

### Requirement: Tool input schemas are closed and every field is required (strict-mode eligible)
Every tool's advertised `input_schema` SHALL, at every level of nesting — not only at the schema's top level — list every key present in that object's `properties` within its own `required` array, and SHALL set `additionalProperties: false`. Optionality SHALL be expressed only via a nullable JSON Schema type (`["T", "null"]`) on an otherwise-required field, or (where the field is itself an array or object whose empty form is a meaningful "unused" value, e.g. `recall_memory_notes`'s `filters`) via an empty value on an otherwise-required, non-nullable field — never via a field's absence from `required`. No schema anywhere SHALL contain a literal `null` entry inside an `enum` array, no schema anywhere SHALL declare a `format` value that is not a standard JSON Schema format recognized by every target platform, and no numeric field anywhere SHALL declare `minimum`, `maximum`, or `multipleOf`. Every tool's schema SHALL therefore be eligible for validation under a JSON-Schema-strict tool-calling mode that enforces closed objects and full `required` lists, with no undocumented exception for any tool.

#### Scenario: Every field, root and nested, is required
- **WHEN** a client calls `tools/list` for any tool
- **THEN** every key present in every object subschema's `properties`, at every nesting level, also appears in that same object's `required` array

#### Scenario: Optional fields are expressed as a nullable type, never as an absent key
- **WHEN** a client calls `tools/list` for a tool with a field that has no mandatory value (for example `recall_memory_notes`'s `query`, or `list_memory_notes`'s `limit`)
- **THEN** the field's schema has `type: ["T", "null"]` for its concrete type `T`, and the field's key is present in that object's `required` array

#### Scenario: No enum ever contains a null entry
- **WHEN** a client calls `tools/list` for any tool with an enum-constrained field
- **THEN** the field's `enum` array lists only its valid string values, with no `null` entry, regardless of whether the field is otherwise required or optional

#### Scenario: Every object subschema is closed against unexpected keys
- **WHEN** a client calls `tools/list` for a tool whose schema contains a nested object (for example an entry in `read_memory_notes`'s `paths`, `write_memory_notes`'s `notes`, `evolve_core_persona`'s `updates`, or `recall_memory_notes`'s `filters`)
- **THEN** both the top-level schema and every nested object subschema have `additionalProperties: false` and a `required` array listing every one of that object's own keys

#### Scenario: No tool declares an unrecognized numeric format
- **WHEN** a client calls `tools/list` for any tool with an integer-typed field
- **THEN** the field's schema carries no `format` value outside the set recognized by every target platform — in practice, Muninn's integer fields declare no `format` value at all

#### Scenario: No numeric field declares a range or multiple-of constraint
- **WHEN** a client calls `tools/list` for any tool with an integer- or number-typed field (for example `list_memory_notes`'s `limit`, or `recall_memory_notes`'s `limit`)
- **THEN** the field's schema carries no `minimum`, `maximum`, or `multipleOf` keyword

#### Scenario: An optional array or object field with no natural nullable form is expressed as required-with-empty-default
- **WHEN** a client calls `tools/list` for `recall_memory_notes`
- **THEN** the `filters` field is present in `required`, has `type: "array"` (not a nullable union), and an empty array is the documented way to supply no property filters

#### Scenario: No tool has an undocumented open-object exception
- **WHEN** a client calls `tools/list` for any tool, including `update_note_properties`
- **THEN** every object subschema in that tool's input schema, including the root, has `additionalProperties: false` — no tool's schema contains an object with an open (caller-defined) key set

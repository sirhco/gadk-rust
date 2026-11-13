// Copyright 2026 Chris Olson
// Portions derived from ADK-Go (https://github.com/google/adk-go), Copyright 2026 Google LLC.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

package main

import (
	"encoding"
	"encoding/json"
	"fmt"
	"reflect"
	"strings"
	"time"

	"github.com/google/jsonschema-go/jsonschema"
	"google.golang.org/genai"
	"gopkg.in/yaml.v3"

	"google.golang.org/adk/v2/model"
)

// fixupsFile is fixups.json (SCENARIO.md "Fixup table").
type fixupsFile struct {
	Roots            []fixupRoot       `json:"roots"`
	Fixups           []fixupEntry      `json:"fixups"`
	Recursion        []pathEntry       `json:"recursion"`
	AnyFields        []pathEntry       `json:"any_fields"`
	Unencodable      []pathEntry       `json:"unencodable"`
	KeyMismatches    []keyMismatch     `json:"key_mismatches"`
	CustomJSONTypes  []string          `json:"custom_json_types"`
	JSONSchemaFields []jsonSchemaField `json:"jsonschema_fields"`
	ShapePaths       []shapePath       `json:"shape_paths"`
	Shapes           map[string]shape  `json:"shapes"`
	ShapeSelfCheck   selfCheck         `json:"shape_selfcheck"`
}

type fixupRoot struct {
	Name   string `json:"name"`
	GoType string `json:"go_type"`
}

type fixupEntry struct {
	Path   string `json:"path"`
	Owner  string `json:"owner"`
	Key    string `json:"key"`
	Class  string `json:"class"`
	GoType string `json:"go_type"`
}

type pathEntry struct {
	Path   string `json:"path"`
	Owner  string `json:"owner"`
	Key    string `json:"key"`
	GoType string `json:"go_type"`
}

type keyMismatch struct {
	Path    string `json:"path"`
	Owner   string `json:"owner"`
	GoName  string `json:"go_name"`
	YAMLKey string `json:"yaml_key"`
	JSONTag string `json:"json_tag"`
	Kind    string `json:"kind"`
}

type jsonSchemaField struct {
	Order       int    `json:"order"`
	GoName      string `json:"go_name"`
	YAMLKey     string `json:"yaml_key"`
	JSONTag     string `json:"json_tag"`
	JSONKeyword string `json:"json_keyword"`
	GoType      string `json:"go_type"`
	Kind        string `json:"kind"`
	Class       string `json:"class"`
}

var (
	bytesType         = reflect.TypeFor[[]byte]()
	durationType      = reflect.TypeFor[time.Duration]()
	timeType          = reflect.TypeFor[time.Time]()
	textMarshalerType = reflect.TypeFor[encoding.TextMarshaler]()
	yamlMarshalerType = reflect.TypeFor[yaml.Marshaler]()
	jsonMarshalerType = reflect.TypeFor[json.Marshaler]()
	schemaType        = reflect.TypeFor[genai.Schema]()
	funcDeclType      = reflect.TypeFor[genai.FunctionDeclaration]()
)

// fixupClass is the fixup class of a value of type t at a YAML position, or
// "" if gadk serde's shape needs no fixup there. The checks follow yaml.v3's
// encoder.marshal order (time.Time, time.Duration, Marshaler,
// TextMarshaler), with []byte (an int sequence) first.
func fixupClass(t reflect.Type) string {
	if (t.Kind() == reflect.Slice || t.Kind() == reflect.Array) && t.Elem().Kind() == reflect.Uint8 {
		return "Bytes"
	}
	base := t
	if base.Kind() == reflect.Pointer {
		base = base.Elem()
	}
	switch {
	case base == timeType:
		return "Time"
	case base == durationType:
		return "Duration"
	case t.Implements(yamlMarshalerType):
		return "YAMLMarshaler"
	case t.Implements(textMarshalerType):
		return "TextMarshaler"
	case base == schemaType:
		return "SchemaTitle"
	}
	return ""
}

type walker struct {
	f          *fixupsFile
	structs    map[string]reflect.Type
	stack      []reflect.Type
	customJSON map[string]bool
}

// walk visits the value position path of static type t, owned by struct
// owner under YAML key key.
func (w *walker) walk(path, owner, key string, t reflect.Type) {
	w.walkDecl(path, owner, key, t, t)
}

// walkDecl is walk with decl, the type as declared at this position (t with
// its pointers removed one by one; a class found on the pointee is reported
// once, with the declared type).
func (w *walker) walkDecl(path, owner, key string, t, decl reflect.Type) {
	if t.Kind() != reflect.Interface && t.Kind() != reflect.Pointer {
		if t.Implements(jsonMarshalerType) || reflect.PointerTo(t).Implements(jsonMarshalerType) {
			if t.PkgPath() != "" {
				w.customJSON[t.String()] = true
			}
		}
	}
	if t.Kind() == reflect.Pointer && fixupClass(t.Elem()) != "" {
		w.walkDecl(path, owner, key, t.Elem(), decl)
		return
	}
	if class := fixupClass(t); class != "" {
		w.f.Fixups = append(w.f.Fixups, fixupEntry{path, owner, key, class, typeName(decl)})
		if class != "SchemaTitle" {
			return
		}
	}
	switch t.Kind() {
	case reflect.Pointer:
		w.walkDecl(path, owner, key, t.Elem(), decl)
	case reflect.Slice, reflect.Array:
		w.walk(path+"[]", owner, key, t.Elem())
	case reflect.Map:
		w.walk(path+"{}", owner, key, t.Elem())
	case reflect.Interface:
		w.f.AnyFields = append(w.f.AnyFields, pathEntry{path, owner, key, typeName(decl)})
	case reflect.Func, reflect.Chan, reflect.Complex64, reflect.Complex128, reflect.UnsafePointer:
		w.f.Unencodable = append(w.f.Unencodable, pathEntry{path, owner, key, typeName(decl)})
	case reflect.Struct:
		w.f.ShapePaths = append(w.f.ShapePaths, shapePath{path, typeName(t)})
		w.structs[typeName(t)] = t
		for _, s := range w.stack {
			if s == t {
				w.f.Recursion = append(w.f.Recursion, pathEntry{path, owner, key, typeName(decl)})
				return
			}
		}
		w.stack = append(w.stack, t)
		defer func() { w.stack = w.stack[:len(w.stack)-1] }()
		for i := 0; i < t.NumField(); i++ {
			sf := t.Field(i)
			if sf.PkgPath != "" && !sf.Anonymous {
				continue // yaml.v3 skips unexported fields
			}
			yamlKey := strings.ToLower(sf.Name)
			fieldPath := path + "." + yamlKey
			jsonTag := sf.Tag.Get("json")
			jsonName, _, _ := strings.Cut(jsonTag, ",")
			switch {
			case sf.Anonymous:
				w.f.KeyMismatches = append(w.f.KeyMismatches, keyMismatch{fieldPath, t.String(), sf.Name, yamlKey, jsonTag, "embedded"})
			case jsonName == "-":
				w.f.KeyMismatches = append(w.f.KeyMismatches, keyMismatch{fieldPath, t.String(), sf.Name, yamlKey, jsonTag, "json_ignored"})
			case jsonName != "" && strings.ToLower(strings.ReplaceAll(jsonName, "_", "")) != yamlKey:
				w.f.KeyMismatches = append(w.f.KeyMismatches, keyMismatch{fieldPath, t.String(), sf.Name, yamlKey, jsonTag, "renamed"})
			}
			if t == funcDeclType && sf.Type.Kind() == reflect.Interface &&
				(sf.Name == "ParametersJsonSchema" || sf.Name == "ResponseJsonSchema") {
				w.f.Fixups = append(w.f.Fixups, fixupEntry{fieldPath, t.String(), yamlKey, "JsonSchema", "*jsonschema.Schema"})
				continue
			}
			w.walk(fieldPath, t.String(), yamlKey, sf.Type)
		}
	}
}

// jsonSchemaKeywords gives the JSON keyword of jsonschema-go's json:"-"
// fields, as Schema.MarshalJSON writes them ("*" = Extra's keys are written
// inline; "" = no keyword: PropertyOrder only orders "properties").
var jsonSchemaKeywords = map[string]string{
	"DependencySchemas": "dependencies",
	"DependencyStrings": "dependencies",
	"Type":              "type",
	"Types":             "type",
	"Items":             "items",
	"ItemsArray":        "items",
	"Extra":             "*",
	"PropertyOrder":     "",
}

// typeName is t.String(), except that json.RawMessage keeps that name (Go
// 1.27 makes it an alias of jsontext.Value, whose String() differs by
// toolchain).
func typeName(t reflect.Type) string {
	if t == reflect.TypeFor[json.RawMessage]() {
		return "json.RawMessage"
	}
	return t.String()
}

func writeFixups(path string, cases []probeCase, results map[string]caseResult) (*fixupsFile, error) {
	f := &fixupsFile{
		Fixups: []fixupEntry{}, Recursion: []pathEntry{}, AnyFields: []pathEntry{},
		Unencodable: []pathEntry{}, KeyMismatches: []keyMismatch{},
	}
	w := &walker{f: f, structs: map[string]reflect.Type{}, customJSON: map[string]bool{}}
	for _, r := range []struct {
		name string
		t    reflect.Type
	}{
		{"LLMRequest", reflect.TypeFor[model.LLMRequest]()},
		{"LLMResponse", reflect.TypeFor[model.LLMResponse]()},
		{"FunctionCall", reflect.TypeFor[genai.FunctionCall]()},
		{"FunctionResponse", reflect.TypeFor[genai.FunctionResponse]()},
	} {
		f.Roots = append(f.Roots, fixupRoot{r.name, r.t.String()})
		w.walk(r.name, "", "", r.t)
	}
	f.CustomJSONTypes = sortedKeys(w.customJSON)

	st := reflect.TypeFor[jsonschema.Schema]()
	for i := 0; i < st.NumField(); i++ {
		sf := st.Field(i)
		if sf.PkgPath != "" {
			continue
		}
		tag := sf.Tag.Get("json")
		keyword, _, _ := strings.Cut(tag, ",")
		if keyword == "-" {
			k, ok := jsonSchemaKeywords[sf.Name]
			if !ok {
				return nil, fmt.Errorf("jsonschema.Schema.%s: json:\"-\" field without a known keyword", sf.Name)
			}
			keyword = k
		}
		f.JSONSchemaFields = append(f.JSONSchemaFields, jsonSchemaField{
			Order: len(f.JSONSchemaFields), GoName: sf.Name, YAMLKey: strings.ToLower(sf.Name),
			JSONTag: tag, JSONKeyword: keyword, GoType: typeName(sf.Type), Kind: sf.Type.Kind().String(),
			Class: fixupClass(sf.Type),
		})
	}
	w.walk("JsonSchema", "", "", st)
	f.Roots = append(f.Roots, fixupRoot{"JsonSchema", typeName(st)})
	f.Shapes = buildShapes(w.structs)
	sc, err := runSelfCheck(f.Shapes, cases, results)
	if err != nil {
		return nil, err
	}
	f.ShapeSelfCheck = sc
	return f, writeJSON(path, f)
}

// Copyright 2026 Chris Olson
// Portions derived from gopkg.in/yaml.v3 (https://github.com/go-yaml/yaml), Copyright 2011-2019 Canonical Ltd.
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

// The parse corpus: the YAML inputs of yaml.v3 v3.0.1's decode_test.go,
// encode_test.go, node_test.go and example_embedded_test.go tables (read
// from the module's own test files with go/ast, so nothing is retyped), then
// the edge corpus (edge.go).

import (
	"fmt"
	"go/ast"
	"go/parser"
	"go/token"
	"os"
	"path/filepath"
	"strconv"
	"strings"
)

// corpusCase is one parse input, written to corpus/parse/<name>.yaml.
type corpusCase struct {
	name string
	data []byte
}

// tableSource names one yaml.v3 test table and where its YAML inputs are:
// field "" is a package-level string var; field "[]" is a []string
// literal assigned to a local variable inside a test function (varName
// "<Func>.<local>"); any other field is the []struct{...} var's field
// holding the input.
type tableSource struct {
	file    string // e.g. "decode_test.go"
	prefix  string // case name prefix, e.g. "decode"
	varName string
	field   string
}

// tableSources lists every yaml.v3 test table whose inputs are YAML text, in
// source order. limit_test.go's limitTests are megabyte-sized
// strings.Repeat inputs; edge.go carries reduced variants instead.
var tableSources = []tableSource{
	{"decode_test.go", "decode", "unmarshalTests", "data"},
	{"decode_test.go", "decode", "decoderTests", "data"},
	{"decode_test.go", "decode", "unmarshalErrorTests", "data"},
	{"decode_test.go", "decode", "unmarshalerTests", "data"},
	{"decode_test.go", "decode", "mergeTests", ""},
	{"decode_test.go", "decode", "mergeTestsNested", ""},
	{"decode_test.go", "decode", "unmarshalNullTests", "input"},
	{"decode_test.go", "decode", "unmarshalStrictTests", "data"},
	{"decode_test.go", "decode", "TestFuzzCrashers.cases", "[]"},
	{"encode_test.go", "encode", "marshalTests", "data"},
	{"encode_test.go", "encode", "marshalerTests", "data"},
	{"node_test.go", "node", "nodeTests", "yaml"},
	{"node_test.go", "node", "setStringTests", "yaml"},
	{"node_test.go", "node", "nodeEncodeDecodeTests", "yaml"},
	{"example_embedded_test.go", "example", "data", ""},
}

// goyamlDir is the yaml.v3 v3.0.1 module directory: $GOYAML_DIR, else
// $(go env GOMODCACHE)/gopkg.in/yaml.v3@v3.0.1.
func goyamlDir() (string, error) {
	if d := os.Getenv("GOYAML_DIR"); d != "" {
		return d, nil
	}
	out, err := runGo("env", "GOMODCACHE")
	if err != nil {
		return "", err
	}
	return filepath.Join(strings.TrimSpace(out), "gopkg.in", "yaml.v3@v3.0.1"), nil
}

// tableCorpus extracts every tableSources input. Case names are
// <prefix>-<varName>-<index> (index zero-padded to 3 digits, "." in
// varName replaced by "-") for table rows and <prefix>-<varName> for string
// vars. node_test.go's "[encode]" /
// "[decode]" markers are stripped (TestNodeRoundtrip strips them too).
func tableCorpus(dir string) ([]corpusCase, error) {
	files := map[string]*ast.File{}
	fset := token.NewFileSet()
	var cases []corpusCase
	for _, src := range tableSources {
		f, ok := files[src.file]
		if !ok {
			var err error
			f, err = parser.ParseFile(fset, filepath.Join(dir, src.file), nil, 0)
			if err != nil {
				return nil, err
			}
			files[src.file] = f
		}
		if src.field == "[]" {
			lit, err := findLocalSlice(f, src.varName)
			if err != nil {
				return nil, fmt.Errorf("%s %s: %v", src.file, src.varName, err)
			}
			for i, elt := range lit.Elts {
				s, err := evalString(elt)
				if err != nil {
					return nil, fmt.Errorf("%s %s[%d]: %v", src.file, src.varName, i, err)
				}
				name := fmt.Sprintf("%s-%s-%03d", src.prefix, strings.ReplaceAll(src.varName, ".", "-"), i)
				cases = append(cases, corpusCase{name, []byte(s)})
			}
			continue
		}
		spec := findVar(f, src.varName)
		if spec == nil {
			return nil, fmt.Errorf("%s: var %s not found", src.file, src.varName)
		}
		if src.field == "" {
			s, err := evalString(spec.Values[0])
			if err != nil {
				return nil, fmt.Errorf("%s %s: %v", src.file, src.varName, err)
			}
			cases = append(cases, corpusCase{src.prefix + "-" + src.varName, []byte(s)})
			continue
		}
		lit, ok := spec.Values[0].(*ast.CompositeLit)
		if !ok {
			return nil, fmt.Errorf("%s %s: not a composite literal", src.file, src.varName)
		}
		pos, err := fieldIndex(lit, src.field)
		if err != nil {
			return nil, fmt.Errorf("%s %s: %v", src.file, src.varName, err)
		}
		for i, elt := range lit.Elts {
			row, ok := elt.(*ast.CompositeLit)
			if !ok {
				return nil, fmt.Errorf("%s %s[%d]: not a composite literal", src.file, src.varName, i)
			}
			expr, err := rowField(row, src.field, pos)
			if err != nil {
				return nil, fmt.Errorf("%s %s[%d]: %v", src.file, src.varName, i, err)
			}
			s, err := evalString(expr)
			if err != nil {
				return nil, fmt.Errorf("%s %s[%d]: %v", src.file, src.varName, i, err)
			}
			if src.varName == "nodeTests" {
				s = strings.TrimPrefix(s, "[decode]")
				s = strings.TrimPrefix(s, "[encode]")
			}
			name := fmt.Sprintf("%s-%s-%03d", src.prefix, src.varName, i)
			cases = append(cases, corpusCase{name, []byte(s)})
		}
	}
	return cases, nil
}

func findVar(f *ast.File, name string) *ast.ValueSpec {
	for _, decl := range f.Decls {
		gd, ok := decl.(*ast.GenDecl)
		if !ok || gd.Tok != token.VAR {
			continue
		}
		for _, s := range gd.Specs {
			vs := s.(*ast.ValueSpec)
			if len(vs.Names) == 1 && vs.Names[0].Name == name && len(vs.Values) == 1 {
				return vs
			}
		}
	}
	return nil
}

// findLocalSlice finds "<Func>.<local>": the composite literal assigned
// with := to local inside function Func.
func findLocalSlice(f *ast.File, path string) (*ast.CompositeLit, error) {
	fn, local, ok := strings.Cut(path, ".")
	if !ok {
		return nil, fmt.Errorf("want <Func>.<local>")
	}
	for _, decl := range f.Decls {
		fd, ok := decl.(*ast.FuncDecl)
		if !ok || fd.Name.Name != fn || fd.Body == nil {
			continue
		}
		var found *ast.CompositeLit
		ast.Inspect(fd.Body, func(n ast.Node) bool {
			as, ok := n.(*ast.AssignStmt)
			if !ok || found != nil || len(as.Lhs) != 1 || len(as.Rhs) != 1 {
				return found == nil
			}
			if id, ok := as.Lhs[0].(*ast.Ident); ok && id.Name == local {
				found, _ = as.Rhs[0].(*ast.CompositeLit)
			}
			return found == nil
		})
		if found == nil {
			return nil, fmt.Errorf("no composite literal assigned to %s", local)
		}
		return found, nil
	}
	return nil, fmt.Errorf("func %s not found", fn)
}

// fieldIndex is the position of field among the table's anonymous struct
// fields (for unkeyed rows); `data, error string` counts as two.
func fieldIndex(lit *ast.CompositeLit, field string) (int, error) {
	arr, ok := lit.Type.(*ast.ArrayType)
	if !ok {
		return 0, fmt.Errorf("not a slice literal")
	}
	st, ok := arr.Elt.(*ast.StructType)
	if !ok {
		return 0, fmt.Errorf("element is not an anonymous struct")
	}
	i := 0
	for _, fl := range st.Fields.List {
		for _, n := range fl.Names {
			if n.Name == field {
				return i, nil
			}
			i++
		}
	}
	return 0, fmt.Errorf("no field %q", field)
}

func rowField(row *ast.CompositeLit, field string, pos int) (ast.Expr, error) {
	if len(row.Elts) > 0 {
		if _, keyed := row.Elts[0].(*ast.KeyValueExpr); keyed {
			for _, e := range row.Elts {
				kv := e.(*ast.KeyValueExpr)
				if id, ok := kv.Key.(*ast.Ident); ok && id.Name == field {
					return kv.Value, nil
				}
			}
			return nil, fmt.Errorf("keyed row without %q", field)
		}
	}
	if pos >= len(row.Elts) {
		return nil, fmt.Errorf("row has %d fields, want index %d", len(row.Elts), pos)
	}
	return row.Elts[pos], nil
}

// evalString evaluates the constant string expressions the tables use:
// literals, +, parentheses and strings.Repeat(s, n).
func evalString(e ast.Expr) (string, error) {
	switch e := e.(type) {
	case *ast.BasicLit:
		if e.Kind != token.STRING {
			return "", fmt.Errorf("not a string literal: %s", e.Value)
		}
		return strconv.Unquote(e.Value)
	case *ast.ParenExpr:
		return evalString(e.X)
	case *ast.BinaryExpr:
		if e.Op != token.ADD {
			return "", fmt.Errorf("unsupported string operator %s", e.Op)
		}
		a, err := evalString(e.X)
		if err != nil {
			return "", err
		}
		b, err := evalString(e.Y)
		if err != nil {
			return "", err
		}
		return a + b, nil
	case *ast.CallExpr:
		sel, ok := e.Fun.(*ast.SelectorExpr)
		if ok && sel.Sel.Name == "Repeat" && len(e.Args) == 2 {
			if pkg, ok := sel.X.(*ast.Ident); ok && pkg.Name == "strings" {
				s, err := evalString(e.Args[0])
				if err != nil {
					return "", err
				}
				n, err := evalInt(e.Args[1])
				if err != nil {
					return "", err
				}
				return strings.Repeat(s, n), nil
			}
		}
	}
	return "", fmt.Errorf("unsupported string expression %T", e)
}

func evalInt(e ast.Expr) (int, error) {
	switch e := e.(type) {
	case *ast.BasicLit:
		if e.Kind != token.INT {
			return 0, fmt.Errorf("not an int literal: %s", e.Value)
		}
		n, err := strconv.ParseInt(e.Value, 0, 64)
		return int(n), err
	case *ast.ParenExpr:
		return evalInt(e.X)
	case *ast.BinaryExpr:
		a, err := evalInt(e.X)
		if err != nil {
			return 0, err
		}
		b, err := evalInt(e.Y)
		if err != nil {
			return 0, err
		}
		switch e.Op {
		case token.ADD:
			return a + b, nil
		case token.SUB:
			return a - b, nil
		case token.MUL:
			return a * b, nil
		case token.QUO:
			return a / b, nil
		}
		return 0, fmt.Errorf("unsupported int operator %s", e.Op)
	}
	return 0, fmt.Errorf("unsupported int expression %T", e)
}

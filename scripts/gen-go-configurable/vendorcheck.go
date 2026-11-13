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
	"bytes"
	"fmt"
	"go/ast"
	"go/parser"
	"go/token"
	"os"
	"path/filepath"
	"strings"
)

// vendoredDecl names one declaration the probe copies verbatim from ADK-Go
// (Go forbids importing another module's internal/ packages).
//
// name selects the declaration: a function name, "Recv.method" for a
// method, a type or variable name (the whole GenDecl holding it is taken, so
// a parenthesized var block is one entry), or "stmts:Func", the longest prefix
// of Func's body statements that mention neither "configurable." nor
// "conformanceTools" (RegisterFunctions' functiontool.New calls on the ADK-Go
// side, newConformanceTools' copy of them on the probe side). The text runs
// from the doc comment (if any) to the end of the declaration.
type vendoredDecl struct {
	adkFile   string
	probeFile string
	name      string
	probeName string // "" = same as name
}

const (
	adkRecording = "internal/configurable/conformance/replayplugin/recording/recording.go"
	adkRecord    = "internal/configurable/conformance/recordplugin/record_plugin.go"
	adkState     = "internal/configurable/conformance/recordplugin/invocation_record_state.go"
	adkYAMLUtils = "internal/configurable/conformance/replayplugin/yaml_utils.go"
	adkReplay    = "internal/configurable/conformance/replayplugin/replay_plugin.go"
	adkFunctions = "internal/configurable/conformance/functions.go"
)

var vendoredDecls = []vendoredDecl{
	{adkRecording, "recording/recording.go", "Recordings", ""},
	{adkRecording, "recording/recording.go", "Recording", ""},
	{adkRecording, "recording/recording.go", "LLMRecording", ""},
	{adkRecording, "recording/recording.go", "ToolRecording", ""},
	{adkRecord, "vendor_record.go", "recordPlugin", ""},
	{adkRecord, "vendor_record.go", "recordPlugin.saveRecordings", ""},
	{adkRecord, "vendor_record.go", "shouldPruneYAMLField", ""},
	{adkRecord, "vendor_record.go", "snakeCaseFieldMap", ""},
	{adkRecord, "vendor_record.go", "init", ""},
	{adkRecord, "vendor_record.go", "findKeyNode", ""},
	{adkRecord, "vendor_record.go", "cleanYAMLNode", ""},
	{adkRecord, "vendor_record.go", "injectSchemaTitles", ""},
	{adkRecord, "vendor_record.go", "toCamelCaseTitle", ""},
	{adkRecord, "vendor_record.go", "sanitizeLLMRequest", ""},
	{adkState, "vendor_record.go", "invocationRecordState", ""},
	{adkState, "vendor_record.go", "newInvocationRecordState", ""},
	{adkYAMLUtils, "vendor_replay.go", "toIgnore", ""},
	{adkYAMLUtils, "vendor_replay.go", "toIgnoreButRecurse", ""},
	{adkYAMLUtils, "vendor_replay.go", "normalizeYAMLNode", ""},
	{adkReplay, "vendor_replay.go", "dataBlockRegex", ""},
	{adkReplay, "vendor_replay.go", "modifyString", ""},
	{adkReplay, "vendor_replay.go", "canonicalizeJSONSubstrings", ""},
	{adkFunctions, "vendor_functions.go", "ValidateEmailArgs", ""},
	{adkFunctions, "vendor_functions.go", "emailRegex", ""},
	{adkFunctions, "vendor_functions.go", "validateEmail", ""},
	{adkFunctions, "vendor_functions.go", "getUserID", ""},
	{adkFunctions, "vendor_functions.go", "CreateBookingArgs", ""},
	{adkFunctions, "vendor_functions.go", "createBooking", ""},
	{adkFunctions, "vendor_functions.go", "FlightPreferences", ""},
	{adkFunctions, "vendor_functions.go", "TripDetails", ""},
	{adkFunctions, "vendor_functions.go", "SearchFlightsArgs", ""},
	{adkFunctions, "vendor_functions.go", "searchFlights", ""},
	{adkFunctions, "vendor_functions.go", "CalculateTripCostArgs", ""},
	{adkFunctions, "vendor_functions.go", "calculateTripCost", ""},
	{adkFunctions, "vendor_functions.go", "reimburseArgs", ""},
	{adkFunctions, "vendor_functions.go", "reimburse", ""},
	{adkFunctions, "vendor_functions.go", "askForApprovalArgs", ""},
	{adkFunctions, "vendor_functions.go", "askForApproval", ""},
	{adkFunctions, "vendor_functions.go", "stmts:RegisterFunctions", "stmts:newConformanceTools"},
}

// extractVendored prints every vendored declaration, in manifest order, as
// "=== <adk file> <name>\n<text>\n". side "adk" reads them from the ADK-Go
// checkout root; side "probe" reads the probe's own copies (paths relative to
// the current directory, the probe module). Both outputs are identical exactly
// when every copy is byte-identical to its ADK-Go original.
func extractVendored(side, root string) error {
	var out bytes.Buffer
	for _, d := range vendoredDecls {
		file, name := filepath.Join(root, d.adkFile), d.name
		if side == "probe" {
			file = d.probeFile
			if d.probeName != "" {
				name = d.probeName
			}
		}
		text, err := extractDecl(file, name)
		if err != nil {
			return err
		}
		fmt.Fprintf(&out, "=== %s %s\n%s\n", d.adkFile, d.name, text)
	}
	_, err := os.Stdout.Write(out.Bytes())
	return err
}

func extractDecl(file, name string) (string, error) {
	src, err := os.ReadFile(file)
	if err != nil {
		return "", err
	}
	fset := token.NewFileSet()
	f, err := parser.ParseFile(fset, file, src, parser.ParseComments)
	if err != nil {
		return "", err
	}
	text := func(from, to token.Pos) string {
		return string(src[fset.Position(from).Offset:fset.Position(to).Offset])
	}
	if fn, ok := strings.CutPrefix(name, "stmts:"); ok {
		for _, decl := range f.Decls {
			fd, ok := decl.(*ast.FuncDecl)
			if !ok || fd.Recv != nil || fd.Name.Name != fn {
				continue
			}
			var last ast.Stmt
			for _, s := range fd.Body.List {
				t := text(s.Pos(), s.End())
				if strings.Contains(t, "configurable.") || strings.Contains(t, "conformanceTools") {
					break
				}
				last = s
			}
			if last == nil {
				return "", fmt.Errorf("%s: %s has no leading statements", file, fn)
			}
			return text(fd.Body.List[0].Pos(), last.End()), nil
		}
		return "", fmt.Errorf("%s: no func %s", file, fn)
	}
	for _, decl := range f.Decls {
		start := decl.Pos()
		switch d := decl.(type) {
		case *ast.FuncDecl:
			got := d.Name.Name
			if d.Recv != nil && len(d.Recv.List) == 1 {
				t := d.Recv.List[0].Type
				if s, ok := t.(*ast.StarExpr); ok {
					t = s.X
				}
				if id, ok := t.(*ast.Ident); ok {
					got = id.Name + "." + got
				}
			}
			if got != name {
				continue
			}
			if d.Doc != nil {
				start = d.Doc.Pos()
			}
			return text(start, d.End()), nil
		case *ast.GenDecl:
			found := false
			for _, spec := range d.Specs {
				switch s := spec.(type) {
				case *ast.TypeSpec:
					found = found || s.Name.Name == name
				case *ast.ValueSpec:
					for _, n := range s.Names {
						found = found || n.Name == name
					}
				}
			}
			if !found {
				continue
			}
			if d.Doc != nil {
				start = d.Doc.Pos()
			}
			return text(start, d.End()), nil
		}
	}
	return "", fmt.Errorf("%s: no declaration %s", file, name)
}

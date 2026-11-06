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

// parity: skipped TestIdentityDecisionMatrix: tabulates Go context.Value identity answers across out-of-module decorators and promotions; Rust contexts are a closed concrete type with no Value bag
// parity: skipped TestEveryContextProducingMethodIsTabulated: reflects over Go context-producing methods for the decorator matrix; no decorators in Rust
// parity: skipped TestPromotedColumnsActuallyDropADecorator: Go method promotion of embedded interfaces; no Rust counterpart
// parity: skipped TestPromotedColumnsCallTheMethodTheyName: Go method promotion of embedded interfaces; no Rust counterpart
// parity: skipped TestDecoratorAnswersForItselfWhileReadingTheEnclosingUsersData: out-of-module decorators cannot exist in Rust
// parity: skipped TestRebindIsTotalWithNoCachedHandle: Go WithAgentContext rebinding to another InvocationContext; Rust has no rebind derivation
// parity: skipped TestRebindDoesNotCarryTheArtifactsHandle: Go WithAgentContext rebinding; Rust has no rebind derivation
// parity: skipped TestAnUnmarkedContextCanAnswerTheKeyItself: Go context.Value identity key; no Rust counterpart
// parity: skipped TestTheKeyValueEscapesToAnyContext: Go context.Value identity key; no Rust counterpart
// parity: skipped TestIdentityFromAHostileSource: Go adkcontext.Source marker answering the key; no Rust counterpart
// parity: skipped TestPromotedSubSchedulerDropsTheDecorator: out-of-module decorators cannot exist in Rust
// parity: skipped TestDeltaDerivationsReachWithICDeltaOnly: Go WithDelta/WithICDelta dispatch on user implementations; Rust applies ContextDelta in one place
// parity: skipped TestNilInvocationDeltaIsSafeOnlyOnceHeld: Go nil InvocationContextDelta on out-of-module invocations; no Rust counterpart

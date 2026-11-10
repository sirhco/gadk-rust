module gadk/gen-go-vertexreplay

go 1.26.10

require (
	cloud.google.com/go v0.123.0
	cloud.google.com/go/aiplatform v1.127.0
	cloud.google.com/go/longrunning v1.3.0
	google.golang.org/genproto/googleapis/rpc v0.0.0-20261005182115-fad411399dd8
	google.golang.org/grpc v1.84.0
	google.golang.org/protobuf v1.36.12
)

require (
	golang.org/x/net v0.61.0 // indirect
	golang.org/x/sys v0.49.0 // indirect
	golang.org/x/text v0.43.0 // indirect
	google.golang.org/genproto v0.0.0-20260715232425-e75dac1f907d // indirect
	google.golang.org/genproto/googleapis/api v0.0.0-20260928230214-8a89bd6388cc // indirect
)

replace google.golang.org/adk/v2 => ../../../adk-go

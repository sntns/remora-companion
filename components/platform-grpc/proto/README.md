# Vendored sntns-platform protos

Client-side subsets of [sntns-platform](https://github.com/sntns/sntns-platform)'s
public gateway APIs, copied from `main` at `6c6af2f`:

| here | upstream |
|---|---|
| `sntns/service/v1/model.proto` | `components/sntns-service-api-proto/sntns/service/v1/model.proto` |
| `sntns/service/remorachannel/v1/channel_service.proto` | `components/sntns-service-remora-channel-gateway-api-proto/sntns/service/remorachannel/v1/channel_service.proto` |
| `sntns/service/iam/v1/user_service.proto` | `components/sntns-service-iam-gateway-api-proto/sntns/service/iam/v1/{user_service,user_model}.proto` |
| `sntns/service/iam/v1/account_service.proto` | `components/sntns-service-iam-gateway-api-proto/sntns/service/iam/v1/{account_service,account_model}.proto` |

What was changed, and why it is safe:

- The `google.api.http` and `protoc-gen-openapiv2` options are dropped. They
  drive the REST gateway and the OpenAPI document, never the gRPC wire
  format, so a client does not need them (and would otherwise need to vendor
  googleapis and grpc-gateway too).
- Only the RPCs and messages `rmra` calls are kept. Package names, service
  names, message names and field numbers are unchanged, so the method paths
  (`/sntns.service.iam.v1.UserService/GetCurrentUser`, ...) and the encoding
  are exactly upstream's. Fields left out of a kept message are skipped as
  unknown fields when decoding, which protobuf guarantees.

When upstream changes one of these services, re-copy the definitions above
and strip them the same way.

#!/usr/bin/env node

import { protocolBuildPreflightPassed } from "../../agent-fabric-protocol/bin/protocol-build-preflight.js";

if (protocolBuildPreflightPassed()) await import("../dist/bin.js");

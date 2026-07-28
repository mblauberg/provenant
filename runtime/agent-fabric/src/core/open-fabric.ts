import { Fabric, type FabricRuntimeOpenOptions } from "./fabric.js";

export async function openFabric(options: FabricRuntimeOpenOptions): Promise<Fabric> {
  return new Fabric(options);
}

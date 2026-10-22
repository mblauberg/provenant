import { toolSchemas } from "./tool-schemas.js";

console.log(JSON.stringify(Object.fromEntries(
  Object.entries(toolSchemas).map(([name, shape]) => [name, Object.keys(shape)]),
)));

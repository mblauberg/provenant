// Schema validation stays with Ajv; no partial JSON Schema implementation.
const fs = require('node:fs');
const Ajv = require('ajv');
const Ajv2020 = require('ajv/dist/2020');
const Ajv2019 = require('ajv/dist/2019');
try {
  const {schema, value, check} = JSON.parse(fs.readFileSync(0, 'utf8'));
  const draft = schema && schema.$schema || '';
  const Validator = draft.includes('2020-12') ? Ajv2020 : draft.includes('2019-09') ? Ajv2019 : Ajv;
  const ajv = new Validator({allErrors: true, strict: true});
  const validate = ajv.compile(schema);
  if (!check && !validate(value)) throw new Error(ajv.errorsText(validate.errors));
} catch (error) {
  process.stderr.write(error.message + '\n');
  process.exitCode = 1;
}

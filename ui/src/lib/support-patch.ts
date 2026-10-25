import type { SupportFact, Term } from './api';

/** RDF Patch keeps stored blank labels through <_:label> and an exact prev header. */
export function patchTerm(term: Term): string {
  const iri = (value: string) =>
    `<${Array.from(value, (character) => {
      const code = character.codePointAt(0)!;
      return code <= 0x20 || '<>"{}|^`\\'.includes(character)
        ? `\\${code <= 0xffff ? 'u' : 'U'}${code.toString(16).padStart(code <= 0xffff ? 4 : 8, '0')}`
        : character;
    }).join('')}>`;
  switch (term.type) {
    case 'uri':
      return iri(term.value);
    case 'bnode':
      if (!/^[A-Za-z0-9_](?:[A-Za-z0-9_.-]*[A-Za-z0-9_-])?$/.test(term.value))
        throw new Error('The support has an invalid blank-node label.');
      return `<_:${term.value}>`;
    case 'literal': {
      const lexical = JSON.stringify(term.value);
      if (term['xml:lang']) {
        if (!/^[A-Za-z]+(?:-[A-Za-z0-9]+)*$/.test(term['xml:lang']))
          throw new Error('The support has an invalid language tag.');
        const direction = term['its:dir'];
        if (direction && direction !== 'ltr' && direction !== 'rtl')
          throw new Error('The support has an invalid base direction.');
        return `${lexical}@${term['xml:lang']}${direction ? `--${direction}` : ''}`;
      }
      return term.datatype ? `${lexical}^^${iri(term.datatype)}` : lexical;
    }
    case 'triple':
      return `<<( ${patchTerm(term.value.subject)} ${patchTerm(term.value.predicate)} ${patchTerm(term.value.object)} )>>`;
  }
}

export function removalPatch(fact: SupportFact, branchId: string, head: number): string {
  if (
    !/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(branchId) ||
    !Number.isSafeInteger(head) ||
    head < 0
  )
    throw new Error('The branch returned an invalid commit identity.');
  const graph = fact.graph === 'default' ? '' : ` ${patchTerm(fact.graph)}`;
  return `H prev <urn:uuid:${branchId}#commit:${head}> .\nTX .\nD ${fact.triple.map(patchTerm).join(' ')}${graph} .\nTC .\n`;
}

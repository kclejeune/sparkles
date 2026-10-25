import hashlib,importlib.util,json,pathlib,re,sys
from collections import defaultdict
source,queries,out,answers_script=map(pathlib.Path,sys.argv[1:])
spec=importlib.util.spec_from_file_location('terms',answers_script);terms=importlib.util.module_from_spec(spec);spec.loader.exec_module(terms)
adj=defaultdict(list);unique=set();digest=hashlib.sha256();lines=0
pattern=re.compile(rb'<http://example.org/person/([0-9]+)> <http://xmlns.com/foaf/0.1/knows> <http://example.org/person/([0-9]+)> \.\n')
with source.open('rb') as f:
 for line in f:
  digest.update(line);unique.add(hashlib.sha256(line).digest());lines+=1
  if b'<http://xmlns.com/foaf/0.1/knows>' in line:
   m=pattern.fullmatch(line)
   if not m:raise ValueError('unknown generator knows triple')
   adj[int(m[1])].append(int(m[2]))
visited={0};todo=[0]
while todo:
 for node in adj.get(todo.pop(),()):
  if node not in visited:visited.add(node);todo.append(node)
record={'oracle_version':1,'method':'source-graph-traversal-and-canonical-generator-line-set','source_sha256':digest.hexdigest(),'input_lines':lines,'distinct_triples':len(unique),'reachable_nodes':len(visited),'queries':{}}
for name,count in [('knows-reach',len(visited)),('count-all',len(unique))]:
 if not (queries/(name+'.rq')).exists():continue
 literal={'type':'literal','value':str(count),'datatype':terms.XSD+'integer'}
 fp={key:terms.canon([(terms.term(literal,by_value),)]) for key,by_value in [('exact',False),('value',True)]}
 record['queries'][name]={'rows':1,**fp,'query_sha256':hashlib.sha256((queries/(name+'.rq')).read_bytes()).hexdigest()}
out.write_text(json.dumps(record,indent=2)+'\n');print(json.dumps(record),flush=True)

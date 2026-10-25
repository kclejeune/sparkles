import hashlib,json,pathlib,sys,time
root=pathlib.Path(sys.argv[1]);source=root/'qlever/raw-answers/q4.qlever.tsv';start=time.monotonic();count=0;examples=[];read_sha=hashlib.sha256()
with source.open('rb') as rows:
 header=next(rows);read_sha.update(header)
 for row in rows:
  read_sha.update(row);count+=1;a,b=row.rstrip(b'\r\n').split(b'\t')
  if a>=b:
   try:first,second=json.loads(a),json.loads(b)
   except (ValueError,TypeError):continue
   if not first<second:
    examples.append({'row':count,'name1':first,'name2':second,'violates_name1_less_name2':True,'raw_tsv':row.decode().rstrip('\r\n')})
    if len(examples)==5:break
out={'witness_version':1,'method':'Find complete-response pairs violating the original string-less-than filter by Unicode codepoint order. Raw-byte candidates are decoded before acceptance.','source_path':str(source),'source_rows_scanned':count,'completed_full_scan':not examples,'examples':examples,'seconds_for_diagnostic_scan':time.monotonic()-start,'prefix_sha256':read_sha.hexdigest()}
(root/'qlever-q4-witness.json').write_text(json.dumps(out,indent=2)+'\n');print(json.dumps(out),flush=True)

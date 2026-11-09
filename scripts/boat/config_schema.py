"""Explicit owned-worker profile ABI. No credentials or default worker identity."""
from pathlib import PurePosixPath
import re
PROFILE_SCHEMA='sparkles.boat.worker-profile'
def valid_sha(value):return isinstance(value,str)and re.fullmatch('[a-f0-9]{64}',value)is not None
def require(ok,message):
    if not ok:raise ValueError(message)
def absolute(value):return isinstance(value,str)and value.startswith('/')and value!='/'and str(PurePosixPath(value))==value and '..'not in PurePosixPath(value).parts and re.fullmatch('/[A-Za-z0-9_./+-]+',value)is not None
def validate_profile(p):
    allowed={'schema','version','admitted_by_Root','profile_review_sha256','worker','worker_uid','worker_home','future_environment','machine','installed_bundle','retained_old_bundle','runtime_members','old_runtime_members','saved_toolchain_manifest_sha256','startup_repositories','tool_versions','mbx_remote_config','bootstrap_contract','stop_contract_sha256'}
    require(isinstance(p,dict)and set(p)==allowed and p['schema']==PROFILE_SCHEMA and p['version']==1,'unknown profile schema/fields')
    require(p['admitted_by_Root']is True and valid_sha(p['profile_review_sha256']),'profile not independently admitted by Root')
    require(isinstance(p['worker'],str)and re.fullmatch('bx_[a-z0-9]+',p['worker']),'explicit owned worker')
    require(type(p['worker_uid'])is int and p['worker_uid']>0 and absolute(p['worker_home']),'owned worker uid/home')
    e=p['future_environment'];require(set(e)=={'id','name','version','version_id','hook_sha256','hook_bytes','setup_blocking'},'environment schema')
    require(all(isinstance(e[k],str)and e[k]for k in ('id','name','version_id'))and type(e['version'])is int and e['version']>0 and valid_sha(e['hook_sha256'])and type(e['hook_bytes'])is int and 0<e['hook_bytes']<=131072 and e['setup_blocking']is True,'explicit pinned environment')
    require(p['machine']=={'type':'large','vcpu':8,'memoryGB':16},'unreviewed machine/cap profile')
    require(p['tool_versions']=={'rust':'1.99.1','mbx':'1.29.1','mise':'2026.8.16'},'toolchain policy changed')
    cache=p['mbx_remote_config'];require(isinstance(cache,dict)and set(cache)=={'url','namespace','mode'}and cache['mode']=='read-only','explicit read-only MBX config')
    require(isinstance(cache['url'],str)and re.fullmatch(r'https://[A-Za-z0-9.-]+(?::[0-9]{1,5})?(?:/[A-Za-z0-9_./%+-]*)?',cache['url'])is not None,'nonsecret HTTPS cache URL')
    require(isinstance(cache['namespace'],str)and re.fullmatch(r'[A-Za-z0-9_.-]+(?:/[A-Za-z0-9_.-]+)*',cache['namespace'])is not None,'nonsecret cache namespace')
    require(p['bootstrap_contract']=='existing_lock_manifest_fence_v1'and valid_sha(p['saved_toolchain_manifest_sha256'])and valid_sha(p['stop_contract_sha256']),'reviewed bootstrap/manifest/stop contract')
    require(absolute(p['installed_bundle'])and absolute(p['retained_old_bundle'])and p['installed_bundle']!=p['retained_old_bundle'],'explicit setup bundles')
    for key in ('runtime_members','old_runtime_members'):
        require(set(p[key])=={'bootstrap.sh','repository-start.sh','toolchain-state.py','tools.toml','expected-cargo-launcher'},'complete setup-member map')
        for name,row in p[key].items():
            require(valid_sha(row.get('sha256'))and row.get('embedded_mode')==(0o700 if name.endswith('.sh')else 0o600),'setup byte/mode pin')
            if 'bytes'in row:require(type(row['bytes'])is int and 0<=row['bytes']<=1048576,'setup byte bound')
    require(set(p['startup_repositories'])=={'default','retained'}and all(absolute(v)for v in p['startup_repositories'].values()),'startup source paths')
    return p

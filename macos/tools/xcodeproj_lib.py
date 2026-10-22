#!/usr/bin/env python3
"""Writes an Xcode 16 project (objectVersion 77) with folder-synchronized groups.

Folder-synchronized groups mean source files are never listed in the project: adding a file to a
target's folder adds it to the target. That keeps this generated project small and diff-friendly.
"""
import hashlib, sys, os, json

def oid(name):
    return hashlib.md5(name.encode()).hexdigest()[:24].upper()

def q(s):
    s = str(s)
    return s if s.replace('_','').replace('.','').replace('/','').isalnum() and s else '"' + s.replace('\\','\\\\').replace('"','\\"') + '"'

def build(spec):
    o = {}  # id -> (comment, dict-text)
    lines = []
    def add(i, body, comment=None):
        lines.append((i, body, comment))
    name = spec['name']
    # package references
    pkg_refs = []
    for p in spec['packages']:
        add(oid('pkgref'+p['path']), f"isa = XCLocalSwiftPackageReference;\n\t\t\trelativePath = {q(p['path'])};", 'XCLocalSwiftPackageReference "%s"' % p['path'])
        pkg_refs.append(oid('pkgref'+p['path']))
    prod_deps = {}
    for p in spec['packages']:
        for prod in p['products']:
            prod_deps[prod] = oid('proddep'+prod)
            add(prod_deps[prod], f"isa = XCSwiftPackageProductDependency;\n\t\t\tpackage = {oid('pkgref'+p['path'])} /* XCLocalSwiftPackageReference \"{p['path']}\" */;\n\t\t\tproductName = {q(prod)};", prod)
    targets = spec['targets']
    group_children = []
    product_refs = []
    target_ids = []
    for t in targets:
        tn = t['name']
        # synchronized root group(s)
        for folder in t['folders']:
            gid = oid('group'+folder)
            if gid not in [l[0] for l in lines]:
                exc = ''
                if t.get('exclude') and folder == t['folders'][0]:
                    eid = oid('exc'+tn)
                    members = ', '.join(q(x) for x in t['exclude'])
                    add(eid, f"isa = PBXFileSystemSynchronizedBuildFileExceptionSet;\n\t\t\tmembershipExceptions = (\n\t\t\t\t{members},\n\t\t\t);\n\t\t\ttarget = {oid('target'+tn)} /* {tn} */;", f"Exceptions for \"{folder}\" folder in \"{tn}\" target")
                    exc = f"\n\t\t\texceptions = (\n\t\t\t\t{eid} /* Exceptions for \"{folder}\" folder in \"{tn}\" target */,\n\t\t\t);"
                add(gid, f"isa = PBXFileSystemSynchronizedRootGroup;{exc}\n\t\t\tpath = {q(folder)};\n\t\t\tsourceTree = \"<group>\";", folder)
                group_children.append((gid, folder))
        # product
        ptype = {'app':'wrapper.application','tool':'compiled.mach-o.executable','unit':'wrapper.cfbundle','ui':'wrapper.cfbundle'}[t['kind']]
        pname = {'app':t.get('product_name',tn)+'.app','tool':tn,'unit':tn+'.xctest','ui':tn+'.xctest'}[t['kind']]
        pid = oid('product'+tn)
        add(pid, f"isa = PBXFileReference;\n\t\t\texplicitFileType = {ptype};\n\t\t\tincludeInIndex = 0;\n\t\t\tpath = {q(pname)};\n\t\t\tsourceTree = BUILT_PRODUCTS_DIR;", pname)
        product_refs.append((pid, pname))
    # targets
    for t in targets:
        tn = t['name']
        tid = oid('target'+tn)
        target_ids.append((tid, tn))
        src, fw, res = oid('src'+tn), oid('fw'+tn), oid('res'+tn)
        add(src, "isa = PBXSourcesBuildPhase;\n\t\t\tbuildActionMask = 2147483647;\n\t\t\tfiles = (\n\t\t\t);\n\t\t\trunOnlyForDeploymentPostprocessing = 0;", 'Sources')
        deps = [d for d in t.get('products', [])]
        files = ''
        for d in deps:
            bid = oid('bf'+tn+d)
            add(bid, f"isa = PBXBuildFile;\n\t\t\tproductRef = {prod_deps[d]} /* {d} */;", f"{d} in Frameworks")
            files += f"\n\t\t\t\t{bid} /* {d} in Frameworks */,"
        add(fw, f"isa = PBXFrameworksBuildPhase;\n\t\t\tbuildActionMask = 2147483647;\n\t\t\tfiles = ({files}\n\t\t\t);\n\t\t\trunOnlyForDeploymentPostprocessing = 0;", 'Frameworks')
        add(res, "isa = PBXResourcesBuildPhase;\n\t\t\tbuildActionMask = 2147483647;\n\t\t\tfiles = (\n\t\t\t);\n\t\t\trunOnlyForDeploymentPostprocessing = 0;", 'Resources')
        phases = [f"{src} /* Sources */", f"{fw} /* Frameworks */", f"{res} /* Resources */"]
        for n, script in enumerate(t.get('scripts', [])):
            sid = oid('script' + tn + str(n))
            body = script['body'].replace('\\', '\\\\').replace('"', '\\"').replace('\n', '\\n')
            add(sid, f"isa = PBXShellScriptBuildPhase;\n\t\t\talwaysOutOfDate = 1;\n\t\t\tbuildActionMask = 2147483647;\n\t\t\tfiles = (\n\t\t\t);\n\t\t\tinputFileListPaths = (\n\t\t\t);\n\t\t\tinputPaths = (\n\t\t\t);\n\t\t\tname = {q(script['name'])};\n\t\t\toutputFileListPaths = (\n\t\t\t);\n\t\t\toutputPaths = (\n\t\t\t);\n\t\t\trunOnlyForDeploymentPostprocessing = 0;\n\t\t\tshellPath = /bin/sh;\n\t\t\tshellScript = \"{body}\";", script['name'])
            phases.append(f"{sid} /* {script['name']} */")
        # build settings
        base = dict(spec.get('target_settings', {}))
        base.update(t['settings'])
        def cfg(cn):
            s = dict(base)
            s.update(t.get('settings_'+cn.lower(), {}))
            body = ''.join(f"\n\t\t\t\t{k} = {q(v)};" for k, v in sorted(s.items()))
            return f"isa = XCBuildConfiguration;\n\t\t\tbuildSettings = {{{body}\n\t\t\t}};\n\t\t\tname = {cn};"
        cd, cr, cl = oid('cfgD'+tn), oid('cfgR'+tn), oid('cfglist'+tn)
        add(cd, cfg('Debug'), 'Debug'); add(cr, cfg('Release'), 'Release')
        add(cl, f"isa = XCConfigurationList;\n\t\t\tbuildConfigurations = (\n\t\t\t\t{cd} /* Debug */,\n\t\t\t\t{cr} /* Release */,\n\t\t\t);\n\t\t\tdefaultConfigurationIsVisible = 0;\n\t\t\tdefaultConfigurationName = Release;", f'Build configuration list for PBXNativeTarget "{tn}"')
        depids = ''
        for dn in t.get('depends', []):
            px, td = oid('proxy'+tn+dn), oid('tdep'+tn+dn)
            add(px, f"isa = PBXContainerItemProxy;\n\t\t\tcontainerPortal = {oid('project')} /* Project object */;\n\t\t\tproxyType = 1;\n\t\t\tremoteGlobalIDString = {oid('target'+dn)};\n\t\t\tremoteInfo = {q(dn)};", 'PBXContainerItemProxy')
            add(td, f"isa = PBXTargetDependency;\n\t\t\ttarget = {oid('target'+dn)} /* {dn} */;\n\t\t\ttargetProxy = {px} /* PBXContainerItemProxy */;", 'PBXTargetDependency')
            depids += f"\n\t\t\t\t{td} /* PBXTargetDependency */,"
        syncs = ''.join(f"\n\t\t\t\t{oid('group'+f)} /* {f} */," for f in t['folders'])
        pkgdeps = ''.join(f"\n\t\t\t\t{prod_deps[d]} /* {d} */," for d in deps)
        ptype = {'app':'com.apple.product-type.application','tool':'com.apple.product-type.tool','unit':'com.apple.product-type.bundle.unit-test','ui':'com.apple.product-type.bundle.ui-testing'}[t['kind']]
        pname = {'app':t.get('product_name',tn)+'.app','tool':tn,'unit':tn+'.xctest','ui':tn+'.xctest'}[t['kind']]
        add(tid, f"isa = PBXNativeTarget;\n\t\t\tbuildConfigurationList = {cl} /* Build configuration list for PBXNativeTarget \"{tn}\" */;\n\t\t\tbuildPhases = (\n\t\t\t\t" + ",\n\t\t\t\t".join(phases) + ",\n\t\t\t);\n\t\t\tbuildRules = (\n\t\t\t);\n\t\t\tdependencies = (" + depids + "\n\t\t\t);\n\t\t\tfileSystemSynchronizedGroups = (" + syncs + "\n\t\t\t);\n\t\t\tname = " + q(tn) + ";\n\t\t\tpackageProductDependencies = (" + pkgdeps + "\n\t\t\t);\n\t\t\tproductName = " + q(tn) + ";\n\t\t\tproductReference = " + oid('product'+tn) + " /* " + pname + " */;\n\t\t\tproductType = \"" + ptype + "\";", tn)
    # groups
    prod_group = oid('productsgroup')
    add(prod_group, "isa = PBXGroup;\n\t\t\tchildren = (" + ''.join(f"\n\t\t\t\t{i} /* {n} */," for i, n in product_refs) + "\n\t\t\t);\n\t\t\tname = Products;\n\t\t\tsourceTree = \"<group>\";", 'Products')
    main = oid('maingroup')
    kids = ''.join(f"\n\t\t\t\t{i} /* {n} */," for i, n in group_children)
    add(main, f"isa = PBXGroup;\n\t\t\tchildren = ({kids}\n\t\t\t\t{prod_group} /* Products */,\n\t\t\t);\n\t\t\tsourceTree = \"<group>\";", None)
    # project config
    pset = dict(spec['project_settings'])
    def pcfg(cn):
        s = dict(pset); s.update(spec.get('project_settings_'+cn.lower(), {}))
        body = ''.join(f"\n\t\t\t\t{k} = {q(v)};" for k, v in sorted(s.items()))
        return f"isa = XCBuildConfiguration;\n\t\t\tbuildSettings = {{{body}\n\t\t\t}};\n\t\t\tname = {cn};"
    pd, pr, pl = oid('pcfgD'), oid('pcfgR'), oid('pcfglist')
    add(pd, pcfg('Debug'), 'Debug'); add(pr, pcfg('Release'), 'Release')
    add(pl, f"isa = XCConfigurationList;\n\t\t\tbuildConfigurations = (\n\t\t\t\t{pd} /* Debug */,\n\t\t\t\t{pr} /* Release */,\n\t\t\t);\n\t\t\tdefaultConfigurationIsVisible = 0;\n\t\t\tdefaultConfigurationName = Release;", f'Build configuration list for PBXProject "{name}"')
    tlist = ''.join(f"\n\t\t\t\t{i} /* {n} */," for i, n in target_ids)
    plist = ''.join(f"\n\t\t\t\t{r} /* XCLocalSwiftPackageReference */," for r in pkg_refs)
    add(oid('project'), f"isa = PBXProject;\n\t\t\tattributes = {{\n\t\t\t\tBuildIndependentTargetsInParallel = 1;\n\t\t\t\tLastSwiftUpdateCheck = 1600;\n\t\t\t\tLastUpgradeCheck = 1600;\n\t\t\t}};\n\t\t\tbuildConfigurationList = {pl} /* Build configuration list for PBXProject \"{name}\" */;\n\t\t\tcompatibilityVersion = \"Xcode 14.0\";\n\t\t\tdevelopmentRegion = en;\n\t\t\thasScannedForEncodings = 0;\n\t\t\tknownRegions = (\n\t\t\t\ten,\n\t\t\t\tBase,\n\t\t\t);\n\t\t\tmainGroup = {main};\n\t\t\tminimizedProjectReferenceProxies = 1;\n\t\t\tpackageReferences = ({plist}\n\t\t\t);\n\t\t\tproductRefGroup = {prod_group} /* Products */;\n\t\t\tprojectDirPath = \"\";\n\t\t\tprojectRoot = \"\";\n\t\t\ttargets = ({tlist}\n\t\t\t);", 'Project object')
    out = ['// !$*UTF8*$!', '{', '\tarchiveVersion = 1;', '\tclasses = {', '\t};', '\tobjectVersion = 77;', '\tobjects = {', '']
    # group by isa for conventional sections
    def isa_of(body): return body.split('isa = ')[1].split(';')[0]
    order = ['PBXBuildFile','PBXContainerItemProxy','PBXFileReference','PBXFileSystemSynchronizedBuildFileExceptionSet','PBXFileSystemSynchronizedRootGroup','PBXFrameworksBuildPhase','PBXGroup','PBXNativeTarget','PBXProject','PBXResourcesBuildPhase','PBXShellScriptBuildPhase','PBXSourcesBuildPhase','PBXTargetDependency','XCBuildConfiguration','XCConfigurationList','XCLocalSwiftPackageReference','XCSwiftPackageProductDependency']
    for isa in order:
        sec = [l for l in lines if isa_of(l[1]) == isa]
        if not sec: continue
        out.append(f'/* Begin {isa} section */')
        for i, body, comment in sorted(sec, key=lambda l: l[0]):
            c = f' /* {comment} */' if comment else ''
            out.append(f'\t\t{i}{c} = {{\n\t\t\t{body}\n\t\t}};')
        out.append(f'/* End {isa} section */')
        out.append('')
    out += ['\t};', f'\trootObject = {oid("project")} /* Project object */;', '}', '']
    return '\n'.join(out)


"""Compile the actual production cleanup_held body against fake owned proxies.

No independent cleanup algorithm: the production method is extracted verbatim.
This proves resource selection/order/refusal, not native Wayland delivery.
"""
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]

class HeldCleanupPolicyTests(unittest.TestCase):
    @unittest.skipUnless(os.environ.get('CUA_HYPRLAND_SOURCE'), 'matching Hyprland source required for native-resource policy extraction')
    def test_foreground_original_surface_release_before_leave(self):
        source_root=Path(os.environ['CUA_HYPRLAND_SOURCE'])
        source=(source_root / 'src/protocols/core/Seat.cpp').read_text()
        manager=(source_root / 'src/managers/SeatManager.cpp').read_text()
        focus=manager.split('void CSeatManager::setPointerFocus(',1)[1].split('void CSeatManager::',1)[0]
        self.assertLess(focus.index('sendLeave('),focus.index('m_state.pointerFocus = surf'))
        self.assertIn('surface->m_events.destroy.listen([this] { sendLeave(); })',source)
        methods=[]
        for name in ['sendLeave','sendButton']:
            match=re.search(r'^void CWLPointerResource::'+name+r'\(.*?^}',source,re.M|re.S)
            self.assertIsNotNone(match)
            methods.append(match.group())
        fixture=(ROOT / 'tests/foreground_owned_release_fixture.cpp').read_text().replace('// NATIVE_RESOURCE_METHODS','\n'.join(methods))
        compiler=shlex.split(os.environ.get('CXX','')) or [shutil.which('clang++-18') or shutil.which('clang++') or 'c++']
        with tempfile.TemporaryDirectory(prefix='cua-foreground-owned-') as directory:
            cpp=Path(directory)/'policy.cpp'; binary=Path(directory)/'policy'; cpp.write_text(fixture)
            build=subprocess.run([*compiler,'-std=c++20','-Wall','-Wextra','-Wpedantic','-Werror',str(cpp),'-o',str(binary)],capture_output=True,text=True,timeout=60)
            self.assertEqual(build.returncode,0,build.stdout+build.stderr)
            result=subprocess.run([str(binary)],capture_output=True,text=True,timeout=10)
            self.assertEqual(result.returncode,0,result.stdout+result.stderr)

    def test_original_owned_resource_cleanup(self):
        source = (ROOT / 'src/input_experiment.cpp').read_text()
        methods=[]
        for name in ['cleanup_held', 'revoke']:
            match = re.search(r'^    void '+name+r'\(.*?^    }', source, re.M | re.S)
            self.assertIsNotNone(match)
            methods.append(match.group())
        fixture = (ROOT / 'tests/held_cleanup_policy_fixture.cpp').read_text().replace('// PRODUCTION_CLEANUP', '\n'.join(methods))
        step=re.search(r'^    void step\(\).*?^    }',source,re.M|re.S).group()
        sweep=step[step.index('        if (lease && lease->dead)'):]
        fixture=fixture.replace('// PRODUCTION_DISCONNECT_SWEEP','    void disconnect_sweep() {\n'+sweep)
        compiler = shlex.split(os.environ.get('CXX', '')) or [shutil.which('clang++-18') or shutil.which('clang++') or 'c++']
        with tempfile.TemporaryDirectory(prefix='cua-held-cleanup-') as directory:
            cpp = Path(directory) / 'policy.cpp'
            binary = Path(directory) / 'policy'
            cpp.write_text(fixture)
            build = subprocess.run([*compiler, '-std=c++20', '-Wall', '-Wextra', '-Wpedantic', '-Werror', '-I', str(ROOT / 'src'), str(cpp), '-o', str(binary)], capture_output=True, text=True, timeout=60)
            self.assertEqual(build.returncode, 0, build.stdout + build.stderr)
            result = subprocess.run([str(binary)], capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

if __name__ == '__main__':
    unittest.main()

#!/usr/bin/env python3
"""Real clipboard/PNG transfer and native GIF/text previews, on a private Xvfb display."""
import hashlib
import os
from pathlib import Path
import re
import subprocess
import tempfile
import time
import tomllib

import avatar_helpers
from voice_smoke import Client, file_consent, free_port, pulse, tone, measure


def wait(predicate, message, timeout=10):
    end=time.monotonic()+timeout
    while time.monotonic()<end:
        if predicate(): return
        time.sleep(.05)
    raise AssertionError(message)


def main():
    project=Path(__file__).resolve().parent.parent
    binary=project/'target/release/remember-me'
    fixture=project/'target/release/examples/clipboard_fixture'
    modules=[];clients=[];owner=None;xserver=None
    with tempfile.TemporaryDirectory(prefix='remember-me-media-') as tmp:
        root=Path(tmp)
        try:
            # The user's actual clipboard is never read or modified.
            display_read,display_write=os.pipe()
            xserver=subprocess.Popen(['Xvfb','-displayfd',str(display_write),'-screen','0','800x600x24','-nolisten','tcp'],
                pass_fds=(display_write,),stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
            os.close(display_write)
            with os.fdopen(display_read) as pipe: display=':'+pipe.readline().strip()
            env=dict(os.environ,DISPLAY=display);env.pop('WAYLAND_DISPLAY',None)
            extra={'DISPLAY':display,'WAYLAND_DISPLAY':None}
            (root/'bin').mkdir()
            opener=root/'bin/xdg-open'
            opener.write_text('#!/bin/sh\nprintf \'%s\\n\' "$@" >> "$RM_OPEN_LOG"\n')
            opener.chmod(0o700)
            names={}
            for who in 'ab':
                for kind in ('mic','out'):
                    name=f'rm_media_{os.getpid()}_{who}_{kind}';names[who,kind]=name
                    modules.append(pulse('load-module','module-null-sink',f'sink_name={name}','rate=48000','channels=1',f'sink_properties=device.description={name}'))
                config=root/who/'remember-me/config.toml';config.parent.mkdir(parents=True)
                config.write_text(f'name = "{who}"\nauto_update = false\ninput_device = "Monitor of {names[who,"mic"]}"\n'
                    f'output_device = "{names[who,"out"]}"\ndownload_dir = "{root/("downloads_"+who)}"\n')
            port=free_port()
            a=Client(binary,root,'a',['--host','-p',str(port)],extra_env=extra);clients.append(a)
            time.sleep(.6)
            b=Client(binary,root,'b',['--connect',f'127.0.0.1:{port}'],graphics=True,extra_env=extra);clients.append(b)
            a.wait_log('host: HELLO');time.sleep(.3);a.key('\r');b.wait_log('joiner: ACCEPT');time.sleep(.4)
            source=root/'clipboard source.png';avatar_helpers.picture(source)
            owner=subprocess.Popen([str(fixture),str(source)],env=env,stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
            assert owner.stdout.readline().strip()=='READY'
            a.key('iЧерновик\x16')  # Ctrl+V, image data rather than a text path.
            a.wait_log('[clipboard] image ')
            pasted=Path(re.search(r'\[clipboard\] image (.+)',a.log()).group(1))
            assert pasted.exists()
            assert not list((root/'downloads_b').glob('*')), 'paste sent data without Enter'
            a.key('\r');b.wait_log('[file] offer '+pasted.name)
            assert not (root/'downloads_b'/pasted.name).exists()
            file_consent(b,pasted.name)
            received=root/'downloads_b'/pasted.name
            wait(received.exists,'PNG download missing')
            assert hashlib.sha256(received.read_bytes()).digest()==hashlib.sha256(pasted.read_bytes()).digest()
            b.key('\x1b[F\r');b.wait_log('[media] loaded '+pasted.name+' image frames=1')
            wait(lambda:any(w>64 for _,w,_ in b.graphics_headers),'PNG was not rendered using native terminal graphics')
            sound=root/'voice.wav';tone(sound,440,1.2)
            measure(root,names['a','mic'],names['b','out'],sound,True,'voice while viewing downloaded PNG')
            b.key('o');wait(lambda:(root/'opened.log').exists() and str(received) in (root/'opened.log').read_text(),'O did not open the original file externally')
            b.key('\x1b')
            print('  real image clipboard → Enter → consent → SHA-256 → native PNG preview + external action: PASS',flush=True)

            gif=root/'animation.gif';subprocess.run([str(fixture),'--gif',str(gif)],check=True)
            a.key('\x1bOR\x1b[200~'+str(gif)+'\x1b[201~\r')
            file_consent(b,gif.name);downloaded=root/'downloads_b'/gif.name
            wait(downloaded.exists,'GIF download missing')
            b.key('\x1b[F\r');b.wait_log('[media] loaded animation.gif image frames=12')
            time.sleep(1.4);b.key(' ');b.wait_log('[media] paused=true');b.key('\x1b[C')
            measure(root,names['a','mic'],names['b','out'],sound,True,'voice while GIF preview is open')
            b.key(' ');b.wait_log('[media] paused=false')
            time.sleep(1.5);deleted=len(b.graphics_deletions);b.key('\x1b')
            wait(lambda:len(b.graphics_deletions)>deleted,'closing GIF did not release terminal image IDs')
            time.sleep(.2)
            b.key('\x1bOQ');time.sleep(.2);b.key('\x1b[A'*26+'\x1b[B'*4+'\r')
            time.sleep(.2);b.key('\x1b');time.sleep(.2)
            b.key('\x1b[F\r');time.sleep(.3);b.key(' ')
            measure(root,names['b','mic'],names['a','out'],sound,True,'PTT space still transmits inside GIF preview')
            b.key('\x1b[32;1:3u')
            measure(root,names['b','mic'],names['a','out'],sound,False,'PTT release inside GIF preview')
            b.key('\x1b');time.sleep(.2)
            print('  received GIF: animation, pause, stepping and voice: PASS',flush=True)

            text=root/'notes.txt';text.write_text('\n'.join(f'{i}: А🙂界 text' for i in range(200)))
            a.key('\x1bOR\x1b[200~'+str(text)+'\x1b[201~\r');file_consent(b,text.name)
            wait(lambda:(root/'downloads_b'/text.name).exists(),'text download missing')
            b.key('\x1b[F\r');b.wait_log('[media] loaded notes.txt text')
            b.key('\x1b[6~\x1b[F\x1b[H\x1b')
            time.sleep(.2)
            # Switch the preference while the call stays connected.
            b.key('\x1bOQ');time.sleep(.2);b.key('\x1b[A'*26+'\x1b[B'*21)
            time.sleep(.15);b.key('\r');time.sleep(.2)
            cfg=tomllib.loads((root/'b/remember-me/config.toml').read_text())
            assert cfg['media_in_terminal'] is False, cfg
            b.key('\x1b')
            time.sleep(.2);before=b.log().count('[media] open ')
            b.key('\x1b[F\r')
            wait(lambda:str(root/'downloads_b'/text.name) in (root/'opened.log').read_text(),'external preference was ignored')
            assert b.log().count('[media] open ')==before
            a.key('\r');time.sleep(.2)
            assert any('Черновик' in p.read_text() for p in (root/'a/remember-me').glob('chat-*.log')),'image paste lost the chat draft'
            print('  text viewer + scrolling + live external preference + preserved draft: PASS',flush=True)
            print('MEDIA: PASS',flush=True)
        except Exception:
            for client in clients: print(client.log()[-4000:])
            raise
        finally:
            if owner:
                owner.communicate('\n',timeout=3)
            for client in reversed(clients):client.close()
            if xserver:xserver.terminate();xserver.wait(timeout=3)
            for module in reversed(modules):pulse('unload-module',module)


if __name__=='__main__':main()

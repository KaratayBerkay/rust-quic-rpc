import 'package:flutter/material.dart';
import 'package:permission_handler/permission_handler.dart';

import 'src/harness_client.dart';
import 'src/voice_call.dart';

void main() => runApp(const VoiceDemoApp());

class VoiceDemoApp extends StatelessWidget {
  const VoiceDemoApp({super.key});

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'RVP Voice Demo',
      theme: ThemeData(colorSchemeSeed: Colors.deepPurple, useMaterial3: true),
      home: const DemoHomePage(),
    );
  }
}

class DemoHomePage extends StatefulWidget {
  const DemoHomePage({super.key});

  @override
  State<DemoHomePage> createState() => _DemoHomePageState();
}

class _DemoHomePageState extends State<DemoHomePage> {
  final TextEditingController _stateUrl =
      TextEditingController(text: 'http://127.0.0.1:7860/state');
  final TextEditingController _host =
      TextEditingController(text: '127.0.0.1');
  final List<String> _logLines = <String>[];

  bool _loopback = true;
  String _user = 'alice';
  bool _connecting = false;
  VoiceCall? _call;

  bool get _live => _call != null;

  void _log(String message) {
    final line =
        '${DateTime.now().toIso8601String().substring(11, 19)} $message';
    // ignore: avoid_print
    print('[rvp] $line');
    if (mounted) {
      setState(() => _logLines.add(line));
    }
  }

  Future<void> _start() async {
    if (_connecting || _live) return;
    setState(() => _connecting = true);
    try {
      final mic = await Permission.microphone.request();
      if (!mic.isGranted) {
        _log('microphone permission denied');
        return;
      }
      final state = await HarnessState.fetch(_stateUrl.text.trim());
      _log('harness: port=${state.port}, '
          'users=${state.grants.keys.join(',')}, '
          'pairs=${state.pairs.length}');
      final call = _loopback
          ? await VoiceCall.loopback(
              state: state,
              host: _host.text.trim(),
              log: _log,
            )
          : await VoiceCall.twoDevice(
              state: state,
              me: _user,
              host: _host.text.trim(),
              log: _log,
            );
      if (!mounted) {
        await call.stop();
        return;
      }
      setState(() => _call = call);
    } catch (e) {
      _log('start failed: $e');
    } finally {
      if (mounted) setState(() => _connecting = false);
    }
  }

  Future<void> _stop() async {
    final call = _call;
    if (call == null) return;
    setState(() => _call = null);
    try {
      await call.stop();
    } catch (e) {
      _log('stop failed: $e');
    }
  }

  @override
  void dispose() {
    _call?.stop();
    _stateUrl.dispose();
    _host.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: const Text('RVP Voice Demo')),
      body: ListView(
        padding: const EdgeInsets.all(16),
        children: [
          TextField(
            controller: _stateUrl,
            decoration: const InputDecoration(
              labelText: 'Harness state URL',
              helperText: 'GET /state from p4a_harness (grants + keys)',
            ),
            enabled: !_live,
          ),
          TextField(
            controller: _host,
            decoration: const InputDecoration(
              labelText: 'Media host',
              helperText: 'Host for QUIC (port comes from the harness)',
            ),
            enabled: !_live,
          ),
          SwitchListTile(
            title: const Text('Loopback (both users on this device)'),
            subtitle: const Text('mic → alice → SFU → bob → speaker'),
            value: _loopback,
            onChanged: _live
                ? null
                : (v) => setState(() => _loopback = v),
          ),
          if (!_loopback)
            DropdownButtonFormField<String>(
              initialValue: _user,
              decoration: const InputDecoration(labelText: 'This device is'),
              items: const [
                DropdownMenuItem(value: 'alice', child: Text('alice')),
                DropdownMenuItem(value: 'bob', child: Text('bob')),
              ],
              onChanged: _live ? null : (v) => setState(() => _user = v!),
            ),
          const SizedBox(height: 12),
          Row(
            children: [
              FilledButton.icon(
                onPressed: _live || _connecting ? null : _start,
                icon: const Icon(Icons.call),
                label: Text(_connecting ? 'Connecting…' : 'Connect'),
              ),
              const SizedBox(width: 12),
              OutlinedButton.icon(
                onPressed: _live ? _stop : null,
                icon: const Icon(Icons.call_end),
                label: const Text('Hang up'),
              ),
              const SizedBox(width: 12),
              IconButton.filledTonal(
                tooltip: 'Mute',
                onPressed: _live
                    ? () => setState(() => _call!.muted = !_call!.muted)
                    : null,
                icon: Icon(
                  _live && _call!.muted ? Icons.mic_off : Icons.mic,
                ),
              ),
            ],
          ),
          if (_live)
            Padding(
              padding: const EdgeInsets.only(top: 8),
              child: Text(
                'On air as ${_call!.user}'
                '${_call!.muted ? ' (muted)' : ''}',
                style: Theme.of(context).textTheme.bodyMedium,
              ),
            ),
          const SizedBox(height: 16),
          Text('Log', style: Theme.of(context).textTheme.titleSmall),
          for (final line in _logLines)
            Text(line, style: Theme.of(context).textTheme.bodySmall),
        ],
      ),
    );
  }
}

import 'package:flutter/material.dart';
import 'package:permission_handler/permission_handler.dart';

import 'src/harness_client.dart';
import 'src/nest_client.dart';
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
  static const _modeHarness = 'harness';
  static const _modeNest = 'nest';

  final TextEditingController _stateUrl =
      TextEditingController(text: 'http://127.0.0.1:7860/state');
  final TextEditingController _host =
      TextEditingController(text: '127.0.0.1');
  final TextEditingController _nestUrl =
      TextEditingController(text: 'http://127.0.0.1:3000');
  final TextEditingController _nestUser =
      TextEditingController(text: 'bob');
  final TextEditingController _roomId = TextEditingController();
  final TextEditingController _inviteUser =
      TextEditingController(text: 'alice');
  final TextEditingController _mediaAddr =
      TextEditingController(text: '127.0.0.1:4433');
  final List<String> _logLines = <String>[];

  String _mode = _modeHarness;
  bool _loopback = true;
  String _user = 'alice';
  bool _connecting = false;
  bool _rotating = false;
  VoiceCall? _call;
  NestClient? _nestClient;
  NestSession? _nestSession;

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

  Future<NestSession> _loginNest() async {
    final url = _nestUrl.text.trim();
    final user = _nestUser.text.trim();
    if (url.isEmpty || user.isEmpty) {
      throw StateError('Nest URL and User ID are required');
    }
    final cached = _nestSession;
    if (cached != null && _nestClient != null && _nestClient!.baseUrl == url) {
      return cached;
    }
    final client = NestClient(url);
    final session = await client.login(user);
    _nestClient = client;
    _nestSession = session;
    return session;
  }

  Future<void> _createRoom() async {
    try {
      final session = await _loginNest();
      final room = await _nestClient!.createRoom(session, 'demo');
      _roomId.text = room.id;
      _log('room created: ${room.id} (${room.name}) — '
          'invite the peer, then have them join with this id');
    } catch (e) {
      _log('create room failed: $e');
    }
  }

  Future<void> _addMember() async {
    try {
      final session = await _loginNest();
      final roomId = _roomId.text.trim();
      final peer = _inviteUser.text.trim();
      if (roomId.isEmpty || peer.isEmpty) {
        throw StateError('Room id and invite user are required');
      }
      await _nestClient!.addMember(session, roomId, peer);
      _log('added $peer to room $roomId');
    } catch (e) {
      _log('add member failed: $e');
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
      final VoiceCall call;
      if (_mode == _modeNest) {
        final session = await _loginNest();
        final roomId = _roomId.text.trim();
        if (roomId.isEmpty) {
          throw StateError('Room id is required (create or join a room first)');
        }
        final address = _mediaAddr.text.trim();
        if (address.isEmpty || !address.contains(':')) {
          throw StateError('Media address must be host:port (rvp-cli QUIC)');
        }
        final grant = await _nestClient!.createGrant(session, roomId);
        call = await VoiceCall.nest(
          client: _nestClient!,
          control: session,
          grant: grant,
          address: address,
          log: _log,
        );
      } else {
        final state = await HarnessState.fetch(_stateUrl.text.trim());
        _log('harness: port=${state.port}, '
            'users=${state.grants.keys.join(',')}, '
            'pairs=${state.pairs.length}');
        call = _loopback
            ? await VoiceCall.loopback(
                state: state,
                host: _host.text.trim(),
                stateUrl: _stateUrl.text.trim(),
                log: _log,
              )
            : await VoiceCall.twoDevice(
                state: state,
                me: _user,
                host: _host.text.trim(),
                stateUrl: _stateUrl.text.trim(),
                log: _log,
              );
      }
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
    _nestUrl.dispose();
    _nestUser.dispose();
    _roomId.dispose();
    _inviteUser.dispose();
    _mediaAddr.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: const Text('RVP Voice Demo')),
      body: ListView(
        padding: const EdgeInsets.all(16),
        children: [
          DropdownButtonFormField<String>(
            initialValue: _mode,
            decoration: const InputDecoration(
              labelText: 'Control plane',
              helperText: 'Nest = plan §5 (same room/keys as the web app)',
            ),
            items: const [
              DropdownMenuItem(
                value: _modeNest,
                child: Text('Nest (plan §5) — cross-device calls'),
              ),
              DropdownMenuItem(
                value: _modeHarness,
                child: Text('Harness (dev stand-in)'),
              ),
            ],
            onChanged: _live ? null : (v) => setState(() => _mode = v!),
          ),
          if (_mode == _modeHarness) ...[
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
          ] else ...[
            TextField(
              controller: _nestUrl,
              decoration: const InputDecoration(
                labelText: 'Nest URL',
                helperText: 'Control plane (plan §5), e.g. http://<server>:3000',
              ),
              enabled: !_live,
            ),
            TextField(
              controller: _nestUser,
              decoration: const InputDecoration(
                labelText: 'User ID',
                helperText: 'Must match the id the peer invited into the room',
              ),
              enabled: !_live,
            ),
            TextField(
              controller: _roomId,
              decoration: const InputDecoration(
                labelText: 'Room id',
                helperText: 'From "Create room" here, or shown by the peer',
              ),
              enabled: !_live,
            ),
            TextField(
              controller: _mediaAddr,
              decoration: const InputDecoration(
                labelText: 'Media address (QUIC)',
                helperText: 'rvp-cli --listen host:port, e.g. 1.2.3.4:4433',
              ),
              enabled: !_live,
            ),
            TextField(
              controller: _inviteUser,
              decoration: const InputDecoration(
                labelText: 'Invite user',
                helperText: 'Add this user id as a member (host does this)',
              ),
              enabled: !_live,
            ),
            const SizedBox(height: 8),
            Row(
              children: [
                OutlinedButton.icon(
                  onPressed: _live || _connecting ? null : _createRoom,
                  icon: const Icon(Icons.add_home),
                  label: const Text('Create room'),
                ),
                const SizedBox(width: 12),
                OutlinedButton.icon(
                  onPressed: _live || _connecting ? null : _addMember,
                  icon: const Icon(Icons.person_add),
                  label: const Text('Add member'),
                ),
              ],
            ),
          ],
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
              const SizedBox(width: 12),
              IconButton.filledTonal(
                tooltip: 'Rotate keys (P7c)',
                onPressed: _live && !_rotating && (_call?.canRotate ?? false)
                    ? () async {
                        setState(() => _rotating = true);
                        try {
                          await _call!.rotateKeys();
                        } catch (e) {
                          _log('rotate failed: $e');
                        } finally {
                          if (mounted) setState(() => _rotating = false);
                        }
                      }
                    : null,
                icon: const Icon(Icons.refresh),
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

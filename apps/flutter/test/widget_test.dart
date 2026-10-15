import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:rvp_voice/main.dart';

void main() {
  testWidgets('renders connection form and disabled hang-up initially',
      (tester) async {
    await tester.pumpWidget(const VoiceDemoApp());

    expect(find.text('RVP Voice Demo'), findsOneWidget);
    expect(
      find.widgetWithText(TextField, 'http://127.0.0.1:7860/state'),
      findsOneWidget,
    );
    expect(find.text('Connect'), findsOneWidget);

    final hangUp = tester.widget<OutlinedButton>(
      find.widgetWithText(OutlinedButton, 'Hang up'),
    );
    expect(hangUp.onPressed, isNull); // nothing to hang up yet
  });
}
